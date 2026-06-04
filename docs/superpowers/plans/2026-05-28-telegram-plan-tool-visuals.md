# Telegram Plan and Tool Visuals Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a persistent verbose-mode Telegram audit card for plan/tool execution and remove duplicate streamed tool-status messages.

**Architecture:** Keep final assistant answer streaming and tool progress rendering as separate paths. Add a pure notifier display model that parses planning tool arguments, renders a live checklist plus recent tool activity, and lets `ToolCallNotifier` edit its existing progress message into a completed persistent summary. Remove the agent-side code that sends formatted tool-status lines into the answer stream.

**Tech Stack:** Rust 2021, Tokio, teloxide, serde_json, anyhow, tracing, existing RustFox agent/tool/notifier modules.

---

## File Map

- Modify: `src/platform/tool_notifier.rs` - extend `ToolEvent::Started`, add plan/tool display state, render live and completed audit text, and make notifier finish persist summaries.
- Modify: `src/agent.rs` - send raw tool arguments to the notifier and remove duplicate status streaming into `stream_token_tx`.
- Optionally modify: `src/platform/telegram.rs` - only if the notifier `finish` signature needs a mutable borrow adjustment; no behavior change should be needed.

No database migration is needed. No `src/tools.rs` behavior change is required for this iteration. Do not commit unless the user explicitly asks for a commit.

---

### Task 1: Add Pure Plan and Tool Display State

**Files:**
- Modify: `src/platform/tool_notifier.rs`

- [x] **Step 1: Add failing display-state tests**

Add these tests inside the existing `#[cfg(test)] mod tests` in `src/platform/tool_notifier.rs`:

```rust
    #[test]
    fn test_tool_display_state_renders_plan_create_as_checklist() {
        let mut state = ToolDisplayState::default();

        state.handle_event(ToolEvent::Started {
            name: "plan_create".to_string(),
            args_preview: "Create test plan".to_string(),
            arguments_json: r#"{"title":"Create test plan","steps":["Gather context","Implement fix"]}"#.to_string(),
        });

        let text = state.format_live();
        assert!(text.contains("Working on your request"), "live header missing: {text}");
        assert!(text.contains("Plan"), "plan section missing: {text}");
        assert!(text.contains("Create test plan"), "plan title missing: {text}");
        assert!(text.contains("[ ] 0. Gather context"), "first step missing: {text}");
        assert!(text.contains("[ ] 1. Implement fix"), "second step missing: {text}");
    }

    #[test]
    fn test_tool_display_state_updates_plan_step_status() {
        let mut state = ToolDisplayState::default();

        state.handle_event(ToolEvent::Started {
            name: "plan_create".to_string(),
            args_preview: "Plan".to_string(),
            arguments_json: r#"{"title":"Plan","steps":["First","Second"]}"#.to_string(),
        });
        state.handle_event(ToolEvent::Started {
            name: "plan_update".to_string(),
            args_preview: "step 1".to_string(),
            arguments_json: r#"{"step_id":1,"status":"in_progress","notes":"working"}"#.to_string(),
        });

        let text = state.format_live();
        assert!(text.contains("[ ] 0. First"), "unchanged step missing: {text}");
        assert!(text.contains("[>] 1. Second -- working"), "updated step missing: {text}");
    }

    #[test]
    fn test_tool_display_state_marks_failed_plan_update_after_completion() {
        let mut state = ToolDisplayState::default();

        state.handle_event(ToolEvent::Started {
            name: "plan_create".to_string(),
            args_preview: "Plan".to_string(),
            arguments_json: r#"{"title":"Plan","steps":["Only step"]}"#.to_string(),
        });
        state.handle_event(ToolEvent::Started {
            name: "plan_update".to_string(),
            args_preview: "step 0".to_string(),
            arguments_json: r#"{"step_id":0,"status":"in_progress"}"#.to_string(),
        });
        state.handle_event(ToolEvent::Completed {
            name: "plan_update".to_string(),
            success: false,
        });

        let text = state.format_live();
        assert!(text.contains("[!] 0. Only step"), "failed step missing: {text}");
    }

    #[test]
    fn test_tool_display_state_renders_generic_tool_activity_without_plan() {
        let mut state = ToolDisplayState::default();

        state.handle_event(ToolEvent::Started {
            name: "read_file".to_string(),
            args_preview: "/tmp/file.txt".to_string(),
            arguments_json: r#"{"path":"/tmp/file.txt"}"#.to_string(),
        });
        state.handle_event(ToolEvent::Completed {
            name: "read_file".to_string(),
            success: true,
        });

        let text = state.format_completed();
        assert!(text.contains("Completed"), "completed header missing: {text}");
        assert!(text.contains("Tool activity"), "tool section missing: {text}");
        assert!(text.contains("Reading a file"), "friendly tool label missing: {text}");
        assert!(text.contains("completed"), "completion state missing: {text}");
        assert!(!text.contains("Plan\n"), "plan section should be omitted: {text}");
    }
```

- [x] **Step 2: Run the new tests and verify they fail**

Run:

```bash
cargo test platform::tool_notifier::tests::test_tool_display_state -- --nocapture
```

Expected: fail to compile because `ToolDisplayState` does not exist and `ToolEvent::Started` has no `arguments_json` field.

- [x] **Step 3: Implement the display model**

In `src/platform/tool_notifier.rs`, replace the `ToolEvent::Started` variant with this shape:

```rust
    Started {
        name: String,
        /// Short safe preview for display.
        args_preview: String,
        /// Full tool arguments for notifier-side parsing. Never render directly.
        arguments_json: String,
    },
```

Add these display-model types below `ToolEvent`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
enum PlanStepStatus {
    Todo,
    InProgress,
    Done,
    Failed,
}

impl PlanStepStatus {
    fn from_tool_status(status: &str) -> Self {
        match status {
            "done" => Self::Done,
            "failed" => Self::Failed,
            "in_progress" => Self::InProgress,
            _ => Self::Todo,
        }
    }

    fn marker(&self) -> &'static str {
        match self {
            Self::Todo => "[ ]",
            Self::InProgress => "[>]",
            Self::Done => "[x]",
            Self::Failed => "[!]",
        }
    }

}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanStepDisplay {
    id: usize,
    description: String,
    status: PlanStepStatus,
    notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanDisplay {
    title: String,
    steps: Vec<PlanStepDisplay>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolActivity {
    name: String,
    args_preview: String,
    done: bool,
    success: bool,
}

#[derive(Debug, Clone, Default)]
struct ToolDisplayState {
    plan: Option<PlanDisplay>,
    active_tool: Option<ToolActivity>,
    recent_tools: Vec<ToolActivity>,
    last_plan_update_step: Option<usize>,
}
```

Add this implementation below the types:

```rust
impl ToolDisplayState {
    const MAX_RECENT_TOOLS: usize = 12;

    fn handle_event(&mut self, event: ToolEvent) {
        match event {
            ToolEvent::Started {
                name,
                args_preview,
                arguments_json,
            } => {
                self.apply_started(name, args_preview, &arguments_json);
            }
            ToolEvent::Completed { name, success } => {
                self.apply_completed(&name, success);
            }
        }
    }

    fn has_activity(&self) -> bool {
        self.plan.is_some() || self.active_tool.is_some() || !self.recent_tools.is_empty()
    }

    fn apply_started(&mut self, name: String, args_preview: String, arguments_json: &str) {
        match name.as_str() {
            "plan_create" => {
                if let Some(plan) = parse_plan_create(arguments_json) {
                    self.plan = Some(plan);
                }
                self.push_tool(ToolActivity {
                    name,
                    args_preview,
                    done: false,
                    success: true,
                });
            }
            "plan_update" => {
                self.last_plan_update_step = parse_plan_update(arguments_json)
                    .and_then(|update| self.apply_plan_update(update));
                self.push_tool(ToolActivity {
                    name,
                    args_preview,
                    done: false,
                    success: true,
                });
            }
            _ => {
                self.push_tool(ToolActivity {
                    name,
                    args_preview,
                    done: false,
                    success: true,
                });
            }
        }
    }

    fn apply_completed(&mut self, name: &str, success: bool) {
        if let Some(entry) = self
            .recent_tools
            .iter_mut()
            .rfind(|entry| entry.name == name && !entry.done)
        {
            entry.done = true;
            entry.success = success;
        }

        if self
            .active_tool
            .as_ref()
            .map(|tool| tool.name.as_str() == name)
            .unwrap_or(false)
        {
            self.active_tool = None;
        }

        if name == "plan_update" && !success {
            if let (Some(plan), Some(step_id)) = (&mut self.plan, self.last_plan_update_step) {
                if let Some(step) = plan.steps.iter_mut().find(|step| step.id == step_id) {
                    step.status = PlanStepStatus::Failed;
                }
            }
        }
    }

    fn apply_plan_update(&mut self, update: PlanStepUpdate) -> Option<usize> {
        let plan = self.plan.as_mut()?;
        let step = plan.steps.iter_mut().find(|step| step.id == update.step_id)?;
        step.status = update.status;
        step.notes = update.notes;
        Some(update.step_id)
    }

    fn push_tool(&mut self, activity: ToolActivity) {
        self.active_tool = Some(activity.clone());
        self.recent_tools.push(activity);
        if self.recent_tools.len() > Self::MAX_RECENT_TOOLS {
            let overflow = self.recent_tools.len() - Self::MAX_RECENT_TOOLS;
            self.recent_tools.drain(0..overflow);
        }
    }

    fn format_live(&self) -> String {
        self.format_with_header("Working on your request", false)
    }

    fn format_completed(&self) -> String {
        self.format_with_header("Completed", true)
    }

    fn format_with_header(&self, header: &str, completed: bool) -> String {
        let mut text = String::from(header);

        if let Some(plan) = &self.plan {
            text.push_str("\n\nPlan");
            if !plan.title.is_empty() {
                text.push_str(&format!("\n{}", plan.title));
            }
            for step in &plan.steps {
                text.push_str(&format!(
                    "\n{} {}. {}",
                    step.status.marker(),
                    step.id,
                    step.description
                ));
                if !step.notes.is_empty() {
                    text.push_str(&format!(" -- {}", step.notes));
                }
            }
        }

        if !completed {
            if let Some(active) = &self.active_tool {
                text.push_str("\n\nCurrent");
                text.push_str(&format!("\nRunning: {}", friendly_tool_name(&active.name)));
            }
        }

        if !self.recent_tools.is_empty() {
            text.push_str("\n\nTool activity");
            for tool in &self.recent_tools {
                let state = if tool.done {
                    if tool.success { "completed" } else { "failed" }
                } else {
                    "running"
                };
                let label = friendly_tool_name(&tool.name);
                if tool.args_preview.is_empty() {
                    text.push_str(&format!("\n- {}: {}", label, state));
                } else {
                    text.push_str(&format!("\n- {}: {} ({})", label, state, tool.args_preview));
                }
            }
        }

        if completed {
            text.push_str("\n\nResult\nFinal answer sent below.");
        }

        text
    }
}

struct PlanStepUpdate {
    step_id: usize,
    status: PlanStepStatus,
    notes: String,
}

fn parse_plan_create(arguments_json: &str) -> Option<PlanDisplay> {
    let value: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Plan")
        .to_string();
    let steps = value.get("steps")?.as_array()?;
    let steps = steps
        .iter()
        .enumerate()
        .map(|(id, step)| PlanStepDisplay {
            id,
            description: step.as_str().unwrap_or("").to_string(),
            status: PlanStepStatus::Todo,
            notes: String::new(),
        })
        .collect();

    Some(PlanDisplay { title, steps })
}

fn parse_plan_update(arguments_json: &str) -> Option<PlanStepUpdate> {
    let value: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    let step_id = value.get("step_id")?.as_u64()? as usize;
    let status = value
        .get("status")
        .and_then(|v| v.as_str())
        .map(PlanStepStatus::from_tool_status)
        .unwrap_or(PlanStepStatus::Todo);
    let notes = value
        .get("notes")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Some(PlanStepUpdate {
        step_id,
        status,
        notes,
    })
}
```

- [x] **Step 4: Run the display-state tests and verify they pass**

Run:

```bash
cargo test platform::tool_notifier::tests::test_tool_display_state -- --nocapture
```

Expected: all four `test_tool_display_state_*` tests pass.

---

### Task 2: Wire `ToolCallNotifier` to the Display State and Persist Completion

**Files:**
- Modify: `src/platform/tool_notifier.rs`
- Optionally modify: `src/platform/telegram.rs`

- [x] **Step 1: Add failing notifier finish tests**

Add these tests inside the existing `#[cfg(test)] mod tests` in `src/platform/tool_notifier.rs`:

```rust
    #[test]
    fn test_notifier_final_status_text_returns_completed_card_when_activity_exists() {
        let mut notifier = ToolCallNotifier::new(Bot::new("TEST_TOKEN"), ChatId(1));
        notifier.display_state.handle_event(ToolEvent::Started {
            name: "read_file".to_string(),
            args_preview: "/tmp/file.txt".to_string(),
            arguments_json: r#"{"path":"/tmp/file.txt"}"#.to_string(),
        });
        notifier.display_state.handle_event(ToolEvent::Completed {
            name: "read_file".to_string(),
            success: true,
        });

        let text = notifier
            .final_status_text()
            .expect("activity should produce a final status card");
        assert!(text.contains("Completed"), "completed header missing: {text}");
        assert!(text.contains("Final answer sent below."), "result line missing: {text}");
        assert!(text.contains("Reading a file"), "tool activity missing: {text}");
    }

    #[test]
    fn test_notifier_final_status_text_is_none_without_activity() {
        let notifier = ToolCallNotifier::new(Bot::new("TEST_TOKEN"), ChatId(1));
        assert!(notifier.final_status_text().is_none());
    }
```

- [x] **Step 2: Run the new notifier tests and verify they fail**

Run:

```bash
cargo test platform::tool_notifier::tests::test_notifier_final_status_text -- --nocapture
```

Expected: fail to compile because `ToolCallNotifier` has no `display_state` field and no `final_status_text()` method.

- [x] **Step 3: Replace notifier log storage with display state**

In `ToolCallNotifier`, replace the `tool_log` field with:

```rust
    display_state: ToolDisplayState,
```

Update `ToolCallNotifier::new()`:

```rust
            display_state: ToolDisplayState::default(),
```

Replace `handle_event()` with:

```rust
    pub async fn handle_event(&mut self, event: ToolEvent) {
        self.display_state.handle_event(event);
        self.edit_message().await;
    }
```

Replace `format_status()` with:

```rust
    fn format_status(&self) -> String {
        self.display_state.format_live()
    }
```

Add this helper near `format_status()`:

```rust
    fn final_status_text(&self) -> Option<String> {
        if self.display_state.has_activity() {
            Some(self.display_state.format_completed())
        } else {
            None
        }
    }
```

Replace `finish()` with:

```rust
    pub async fn finish(&mut self) {
        let Some(ref msg) = self.status_msg else {
            return;
        };

        let Some(text) = self.final_status_text() else {
            self.bot.delete_message(self.chat_id, msg.id).await.ok();
            return;
        };

        match self
            .bot
            .edit_message_text(self.chat_id, msg.id, &text)
            .await
        {
            Ok(_) => self.last_edit = Some(Instant::now()),
            Err(e) => debug!("Failed to edit final tool notifier message: {:#}", e),
        }
    }
```

Remove or rewrite the old `format_final()` helper and tests that depend on direct `tool_log` access. Keep the existing `format_args_preview()` and `friendly_tool_name()` tests.

If the compiler reports that `notifier.finish().await` requires a mutable borrow in `src/platform/telegram.rs`, update only that call site inside the spawned task to keep using the existing mutable `notifier` binding:

```rust
            notifier.finish().await;
```

No larger Telegram streaming changes are expected in this task.

- [x] **Step 4: Run notifier tests and verify they pass**

Run:

```bash
cargo test platform::tool_notifier -- --nocapture
```

Expected: all notifier tests pass.

---

### Task 3: Stop Streaming Tool Status Into the Assistant Answer

**Files:**
- Modify: `src/agent.rs`
- Modify: `src/platform/tool_notifier.rs` only if compile errors reveal missed `ToolEvent::Started` construction sites.

- [x] **Step 1: Add a failing source-inspection regression test**

Add this test inside the existing `#[cfg(test)] mod tests` in `src/agent.rs`:

```rust
    #[test]
    fn test_tool_status_is_not_streamed_to_answer_channel() {
        let source = include_str!("agent.rs");
        let status_line_call = ["format_tool_status", "_line("].concat();
        let stream_status_var = ["stream", "_status_tx"].concat();

        assert!(
            !source.contains(&status_line_call),
            "agent.rs must not format tool-status lines for the assistant answer stream"
        );
        assert!(
            !source.contains(&stream_status_var),
            "agent.rs must not clone a separate stream-status sender for tool progress"
        );
    }
```

- [x] **Step 2: Run the regression test and verify it fails**

Run:

```bash
cargo test agent::tests::test_tool_status_is_not_streamed_to_answer_channel -- --nocapture
```

Expected: fail because `src/agent.rs` still contains `stream_status_tx` and calls `format_tool_status_line()`.

- [x] **Step 3: Remove the duplicate stream-status path and send raw arguments to the notifier**

In `src/agent.rs`, remove this block near the start of the agent loop:

```rust
        // Clone the stream sender so tool status can be pushed into the same Telegram
        // message during tool execution, before the final response starts streaming.
        let stream_status_tx = stream_token_tx.clone();
```

In the tool-start notification block, replace the `ToolEvent::Started` construction with:

```rust
                        if let Some(ref tx) = tool_event_tx {
                            let _ =
                                tx.try_send(crate::platform::tool_notifier::ToolEvent::Started {
                                    name: tool_call.function.name.clone(),
                                    args_preview: args_preview.clone(),
                                    arguments_json: tool_call.function.arguments.clone(),
                                });
                        }
```

Remove the entire block that streams formatted tool status into `stream_status_tx`:

```rust
                        // Stream tool status into the Telegram message only when
                        // tool-progress notifications are enabled, to avoid
                        // prepending status lines to otherwise silent/final output.
                        if tool_event_tx.is_some() {
                            if let Some(ref tx) = stream_status_tx {
                                let status =
                                    crate::platform::tool_notifier::format_tool_status_line(
                                        &tool_call.function.name,
                                        &args_preview,
                                    );
                                tx.try_send(status).ok();
                            }
                        }
```

- [x] **Step 4: Run the agent regression test and targeted notifier tests**

Run:

```bash
cargo test agent::tests::test_tool_status_is_not_streamed_to_answer_channel -- --nocapture
cargo test platform::tool_notifier -- --nocapture
```

Expected: both commands pass.

---

### Task 4: Verify End-to-End Behavior and Clean Up Tests

**Files:**
- Modify: `src/platform/tool_notifier.rs` if legacy tests still assume deleted summaries or old `tool_log` structure.
- Modify: `src/agent.rs` only if the regression test needs a more precise source-inspection needle.

- [x] **Step 1: Run formatting**

Run:

```bash
cargo fmt --all -- --check
```

Expected: pass. If it fails, run `cargo fmt --all`, then run the check again.

- [x] **Step 2: Run the full test suite**

Run:

```bash
cargo test
```

Expected: pass. If any failure is unrelated to the notifier/agent changes, record the failing test name and reason before deciding whether to touch it.

- [x] **Step 3: Run clippy**

Run:

```bash
cargo clippy -- -D warnings
```

Expected: pass with no warnings.

- [x] **Step 4: Manual behavior review without Telegram network calls**

Inspect the final code paths and confirm these statements are true:

```text
- Verbose mode still creates a ToolCallNotifier in src/platform/telegram.rs.
- Agent tool execution still sends ToolEvent::Started and ToolEvent::Completed.
- ToolEvent::Started includes raw arguments_json for parser use.
- src/agent.rs does not send format_tool_status_line output into stream_token_tx.
- ToolCallNotifier::finish edits the existing status message into a completed card when activity exists.
- Non-verbose mode still uses the existing Thinking placeholder and final answer stream.
```

- [x] **Step 5: Update implementation notes in the final response**

When reporting completion, mention:

```text
Implemented persistent verbose audit cards for plan/tool progress.
Fixed duplicate lingering tool messages by removing agent-side tool-status streaming.
Verified with cargo fmt, cargo test, and cargo clippy.
```

If any verification command could not be run, state that clearly with the reason.

---

## Self-Review Checklist

- Spec coverage: persistent verbose audit card, planning checklist display, generic tool activity, separate final answer stream, non-verbose unchanged, and duplicate-message root cause are covered by Tasks 1-4.
- Test coverage: formatter logic is unit-tested, final card text is unit-tested, and the duplicate stream path is guarded by an agent source-inspection regression test.
- Type consistency: `ToolEvent::Started` consistently carries `name`, `args_preview`, and `arguments_json`; `ToolDisplayState` owns all plan/tool rendering state; `ToolCallNotifier` owns Telegram message editing only.
- Repo policy: this plan does not include git commit steps because commits require explicit user instruction in this environment.
