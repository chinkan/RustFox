# Steer Messages, /btw Parallel & Markdown Upgrade — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement three independent features: (A) Markdown entities upgrade with blockquote/spoiler/underline/lists, (B) lightweight /btw with single LLM call, (C) steer message system with configurable MidRunMode.

**Architecture:** All changes are edits to existing files. No new files. Feature A is isolated to `src/utils/markdown_entities.rs`. Features B and C touch `src/agent.rs` and `src/platform/telegram.rs` with no logical dependency on A.

**Tech Stack:** Rust, teloxide, pulldown-cmark, regex (already in Cargo.toml)

---

### Task 1: Blockquote entity support

**Files:**
- Modify: `src/utils/markdown_entities.rs`
- Test: inline (`mod tests` block)

- [ ] **Step 1: Add `use teloxide::types::MessageEntityKind` import**

```rust
// At top of markdown_entities.rs, add to existing use block:
use teloxide::types::{MessageEntity, MessageEntityKind};
```

- [ ] **Step 2: Add `blockquote_start` tracking variable**

After `let mut in_blockquote = false;` (line 57), add:
```rust
let mut blockquote_start: Option<usize> = None;
```

- [ ] **Step 3: Set `blockquote_start` in `Tag::BlockQuote` handler**

In the `Event::Start(tag)` match, replace:
```rust
Tag::BlockQuote(_) => {
    in_blockquote = true;
}
```
with:
```rust
Tag::BlockQuote(_) => {
    in_blockquote = true;
    blockquote_start = Some(plain_utf16_len);
}
```

- [ ] **Step 4: Remove `> ` prefix from `Event::Text` blockquote rendering**

Replace the `in_blockquote` branch in `Event::Text` (lines 63-75):
```rust
if in_blockquote {
    let quoted: String = text
        .lines()
        .map(|line| format!("> {}", line))
        .collect::<Vec<_>>()
        .join("\n");
    plain.push_str(&quoted);
    plain_utf16_len += quoted.encode_utf16().count();
} else {
    plain.push_str(&text);
    plain_utf16_len += text.encode_utf16().count();
}
```
with just plain text (no prefix):
```rust
plain.push_str(&text);
plain_utf16_len += text.encode_utf16().count();
```

- [ ] **Step 5: Emit `Blockquote` entity on `TagEnd::BlockQuote`**

Replace the `TagEnd::BlockQuote(_)` handler (start of line 217):
```rust
TagEnd::BlockQuote(_) => {
    in_blockquote = false;
    if !plain.ends_with('\n') {
        plain.push('\n');
        plain_utf16_len += 1;
    }
}
```
with:
```rust
TagEnd::BlockQuote(_) => {
    in_blockquote = false;
    if let Some(start) = blockquote_start.take() {
        let length = plain_utf16_len.saturating_sub(start);
        if length > 0 {
            entities.push(MessageEntity {
                kind: MessageEntityKind::Blockquote,
                offset: start,
                length,
            });
        }
    }
    if !plain.ends_with('\n') {
        plain.push('\n');
        plain_utf16_len += 1;
    }
}
```

- [ ] **Step 6: Update `test_blockquote_prefixes_with_gt` test**

Replace the existing test (lines 643-653):
```rust
#[test]
fn test_blockquote_emits_entity() {
    let (text, entities) = markdown_to_entities("> This is a quote");
    assert!(
        text.contains("This is a quote"),
        "blockquote text must be present: {text}"
    );
    assert!(
        !text.contains("> "),
        "blockquote must NOT have '> ' prefix when entity is used"
    );
    let blockquote = entities.iter().find(|e| {
        matches!(e.kind, MessageEntityKind::Blockquote)
    });
    assert!(
        blockquote.is_some(),
        "blockquote must produce a Blockquote entity"
    );
}
```

- [ ] **Step 7: Run tests to verify**

Run: `cargo test -p rustfox markdown_entities -- --test-threads=1`
Expected: ALL tests pass (including updated blockquote test)

- [ ] **Step 8: Commit**

```bash
git add src/utils/markdown_entities.rs
git commit -m "feat(markdown): add Blockquote entity for Telegram Bot API 7.0+"
```

---

### Task 2: Spoiler and Underline support

**Files:**
- Modify: `src/utils/markdown_entities.rs`
- Test: inline

- [ ] **Step 1: Add sentinel constants and helper functions**

After the last import line, add:
```rust
/// Private Use Area sentinels for Telegram-specific inline formatting.
/// These characters cannot appear in valid Markdown but are valid UTF-8.
const SPOILER_START: char = '\u{E000}';
const SPOILER_END: char = '\u{E001}';
const UL_START: char = '\u{E002}';
const UL_END: char = '\u{E003}';

/// Pre-process markdown before pulldown-cmark parsing:
/// 1. `<u>text</u>` → `\u{E002}Utext\u{E003}/u`
/// 2. `||text||` → `\u{E000}Stext\u{E001}/s`
fn preprocess_markdown(md: &str) -> String {
    let md = md
        .replace("<u>", {
            let mut s = String::with_capacity(2);
            s.push(UL_START);
            s.push('U');
            s
        })
        .replace("</u>", {
            let mut s = String::with_capacity(3);
            s.push(UL_END);
            s.push('/');
            s.push('u');
            s
        });
    let re = regex::Regex::new(r"\|\|(.*?)\|\|").unwrap();
    re.replace_all(&md, {
            let mut prefix = String::with_capacity(2);
            prefix.push(SPOILER_START);
            prefix.push('S');
            let mut suffix = String::with_capacity(3);
            suffix.push(SPOILER_END);
            suffix.push('/');
            suffix.push('s');
            format!("{}$1{}", prefix, suffix)
        })
        .to_string()
}

/// Post-process: remove sentinel markers from plain text, emit spoiler & underline entities.
/// Requires `use teloxide::types::MessageEntityKind;` in scope.
fn postprocess_entities(plain: &mut String, entities: &mut Vec<MessageEntity>) {
    let mut utf16_offset = 0usize;
    let mut out = String::new();
    let mut stack: Vec<(MessageEntityKind, usize)> = Vec::new();
    let chars: Vec<char> = plain.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            c if c == SPOILER_START && i + 1 < chars.len() && chars[i + 1] == 'S' => {
                stack.push((MessageEntityKind::Spoiler, utf16_offset));
                i += 2;
                continue;
            }
            c if c == SPOILER_END
                && i + 2 < chars.len()
                && chars[i + 1] == '/'
                && chars[i + 2] == 's' =>
            {
                if let Some(idx) = stack
                    .iter()
                    .rposition(|(k, _)| *k == MessageEntityKind::Spoiler)
                {
                    let (_, start) = stack.remove(idx);
                    let len = utf16_offset - start;
                    if len > 0 {
                        entities.push(MessageEntity::spoiler(start, len));
                    }
                }
                i += 3;
                continue;
            }
            c if c == UL_START && i + 1 < chars.len() && chars[i + 1] == 'U' => {
                stack.push((MessageEntityKind::Underline, utf16_offset));
                i += 2;
                continue;
            }
            c if c == UL_END
                && i + 2 < chars.len()
                && chars[i + 1] == '/'
                && chars[i + 2] == 'u' =>
            {
                if let Some(idx) = stack
                    .iter()
                    .rposition(|(k, _)| *k == MessageEntityKind::Underline)
                {
                    let (_, start) = stack.remove(idx);
                    let len = utf16_offset - start;
                    if len > 0 {
                        entities.push(MessageEntity::underline(start, len));
                    }
                }
                i += 3;
                continue;
            }
            _ => {
                out.push(chars[i]);
                utf16_offset += chars[i].len_utf16();
                i += 1;
            }
        }
    }
    *plain = out;
}
```

- [ ] **Step 2: Wire `preprocess_markdown` at the start of `markdown_to_entities`**

At line 44, change:
```rust
let parser = Parser::new_ext(markdown, options);
```
to:
```rust
let processed = preprocess_markdown(markdown);
let parser = Parser::new_ext(&processed, options);
```

- [ ] **Step 3: Wire `postprocess_entities` before the return**

Before the final `(plain, entities)` return (line 253), add:
```rust
postprocess_entities(&mut plain, &mut entities);
```

- [ ] **Step 4: Add spoiler/underline tests**

In the `mod tests` block, add:
```rust
#[test]
fn test_spoiler_converts_to_entity() {
    let (text, entities) = markdown_to_entities("||hidden||");
    assert_eq!(text, "hidden");
    assert!(entities.iter().any(|e| matches!(e.kind, MessageEntityKind::Spoiler)));
}

#[test]
fn test_underline_converts_to_entity() {
    let (text, entities) = markdown_to_entities("<u>underlined</u>");
    assert_eq!(text, "underlined");
    assert!(entities.iter().any(|e| matches!(e.kind, MessageEntityKind::Underline)));
}

#[test]
fn test_spoiler_with_bold() {
    let (text, entities) = markdown_to_entities("**bold** and ||spoiler||");
    assert!(text.contains("bold"));
    assert!(text.contains("spoiler"));
    assert!(entities.iter().any(|e| matches!(e.kind, MessageEntityKind::Bold)));
    assert!(entities.iter().any(|e| matches!(e.kind, MessageEntityKind::Spoiler)));
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p rustfox markdown_entities -- --test-threads=1`
Expected: ALL tests pass

- [ ] **Step 6: Commit**

```bash
git add src/utils/markdown_entities.rs
git commit -m "feat(markdown): add Spoiler and Underline entity support via sentinel pre-processing"
```

---

### Task 3: List formatting with bullet/number prefixes

**Files:**
- Modify: `src/utils/markdown_entities.rs`
- Test: inline

- [ ] **Step 1: Add list tracking state variables**

After `let mut in_blockquote = false;` (line 57), add:
```rust
let mut list_counter: Option<usize> = None; // None = unordered, Some(n) = ordered
let mut needs_list_prefix = false;          // true before Item's first Text
```

- [ ] **Step 2: Handle `Tag::List` and `Tag::Item` in `Event::Start`**

In the `Event::Start(tag)` match, add before `_ => {}`:
```rust
Tag::List { start } => {
    list_counter = start;
}
Tag::Item => {
    needs_list_prefix = true;
}
```

- [ ] **Step 3: Inject prefix in `Event::Text` when `needs_list_prefix` is true**

Inside `Event::Text`, before `plain.push_str(&text);`, add:
```rust
if needs_list_prefix {
    let prefix: String = match list_counter {
        None => "• ".to_string(),
        Some(ref mut n) => {
            let p = format!("{}. ", n);
            *n += 1;
            p
        }
    };
    plain.push_str(&prefix);
    plain_utf16_len += prefix.encode_utf16().count();
    needs_list_prefix = false;
}
```

- [ ] **Step 4: Handle `TagEnd::Item` and `TagEnd::List`**

In `Event::End`, update `TagEnd::Item` (line 213-216):
```rust
TagEnd::Item => {
    plain.push('\n');
    plain_utf16_len += 1;
    // needs_list_prefix stays false — it was already consumed in Text
}
```

Add before `_ => {}`:
```rust
TagEnd::List(_) => {
    list_counter = None;
    needs_list_prefix = false;
}
```

- [ ] **Step 5: Add list rendering tests**

```rust
#[test]
fn test_unordered_list_renders_with_bullets() {
    let input = "- item one\n- item two";
    let (text, _) = markdown_to_entities(input);
    assert!(
        text.contains("• item one"),
        "unordered list must use bullet: {text}"
    );
    assert!(
        text.contains("• item two"),
        "second item must also have bullet: {text}"
    );
}

#[test]
fn test_ordered_list_renders_with_numbers() {
    let input = "1. first\n2. second";
    let (text, _) = markdown_to_entities(input);
    assert!(
        text.contains("1. first"),
        "ordered list must use number: {text}"
    );
    assert!(
        text.contains("2. second"),
        "second item must use next number: {text}"
    );
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p rustfox markdown_entities -- --test-threads=1`
Expected: ALL tests pass

- [ ] **Step 7: Commit**

```bash
git add src/utils/markdown_entities.rs
git commit -m "feat(markdown): add list rendering with bullet and number prefixes"
```

---

### Task 4: Remove old `ask_parallel`, add `ask_parallel_lightweight`

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Add `ask_parallel_lightweight` method to Agent**

In `src/agent.rs`, replace the existing `ask_parallel` method (starts at line 2601) with the lightweight version:

```rust
    /// Ask a parallel question while the main agent is processing.
    /// Single LLM call, no tools, no DB access — truly parallel, zero lock contention.
    /// Answer is ephemeral and NOT saved to conversation history.
    pub async fn ask_parallel_lightweight(&self, question: &str) -> Result<String> {
        let system = format!(
            "Answer the user's side question concisely from your knowledge. \
             You have NO tools available. Respond in a single message. \
             Current time: {}",
            self.build_system_context().await,
        );
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::from_text(system)),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::from_text(question.to_string())),
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        let response = self.llm.chat(&messages, &[]).await?;
        Ok(response
            .content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap_or_default())
    }
```

- [ ] **Step 2: Build and verify compilation**

Run: `cargo check`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/agent.rs
git commit -m "feat(btw): replace ask_parallel with lightweight single-LLM-call version"
```

---

### Task 5: Update `/btw` handler in telegram.rs to use lightweight version

**Files:**
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Replace `ask_parallel` call with `ask_parallel_lightweight`**

In the `/btw` handler (lines 816-830), change:
```rust
tokio::spawn(async move {
    match agent_clone.ask_parallel(&btw_text).await {
```
to:
```rust
tokio::spawn(async move {
    match agent_clone.ask_parallel_lightweight(&btw_text).await {
```

- [ ] **Step 2: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat(btw): use ask_parallel_lightweight for true parallel side questions"
```

---

### Task 6: Add `MidRunMode` enum and persistence helpers

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Add `MidRunMode` enum**

Near the top of `src/agent.rs`, after the last import, add:
```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MidRunMode {
    Steer,
    Queue,
}

impl MidRunMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            MidRunMode::Steer => "steer",
            MidRunMode::Queue => "queue",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "steer" => Some(MidRunMode::Steer),
            "queue" => Some(MidRunMode::Queue),
            _ => None,
        }
    }
}
```

- [ ] **Step 2: Add `get_mid_run_mode` helper to Agent**

Add to `impl Agent` block (near other accessors like `queue_injection`):
```rust
    /// Get the current MidRunMode for a user. Defaults to Steer.
    pub async fn get_mid_run_mode(&self, user_id: &str) -> MidRunMode {
        let key = format!("mid_run_mode_{}", user_id);
        self.memory
            .recall("settings", &key)
            .await
            .ok()
            .flatten()
            .and_then(|v| MidRunMode::from_str(&v))
            .unwrap_or(MidRunMode::Steer)
    }

    /// Set the MidRunMode for a user.
    pub async fn set_mid_run_mode(&self, user_id: &str, mode: MidRunMode) {
        let key = format!("mid_run_mode_{}", user_id);
        self.memory
            .remember("settings", &key, mode.as_str(), None)
            .await
            .ok();
    }

    /// Delete the MidRunMode for a user (resets to default).
    pub async fn delete_mid_run_mode(&self, user_id: &str) {
        let key = format!("mid_run_mode_{}", user_id);
        self.memory.forget("settings", &key).await.ok();
    }
```

- [ ] **Step 3: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 4: Commit**

```bash
git add src/agent.rs
git commit -m "feat(steer): add MidRunMode enum with persistence helpers"
```

---

### Task 7: Add `/mode` command handler in telegram.rs

**Files:**
- Modify: `src/platform/telegram.rs`

- [ ] **Step 1: Register `/mode` in `supported_commands()`**

In `supported_commands()` (line 96), add before `BotCommand::new("stop"`:
```rust
BotCommand::new("mode", "Set steer/queue mode for mid-processing messages"),
```

- [ ] **Step 2: Add `/mode` handler before `/stop` handler**

In `handle_message`, add before the `/stop` check (line 977):
```rust
    // Handle /mode command
    if text.starts_with("/mode") {
        let parts: Vec<&str> = text.splitn(2, |c: char| c.is_whitespace()).collect();
        let sub = parts.get(1).copied().unwrap_or("");
        if sub == "steer" {
            agent.set_mid_run_mode(&user_id.to_string(), MidRunMode::Steer).await;
            return send_markdown_message(
                &bot, msg.chat.id,
                "🔄 **Mode set to steer.** Mid-processing messages will be injected as steering context.",
            ).await;
        } else if sub == "queue" {
            agent.set_mid_run_mode(&user_id.to_string(), MidRunMode::Queue).await;
            return send_markdown_message(
                &bot, msg.chat.id,
                "🔄 **Mode set to queue.** Mid-processing messages will wait for the next turn.",
            ).await;
        } else if sub.is_empty() {
            let current = agent.get_mid_run_mode(&user_id.to_string()).await;
            let mode_str = current.as_str();
            return send_markdown_message(
                &bot, msg.chat.id,
                &format!("Current mode: **{}**\n\nUse `/mode steer` or `/mode queue` to change.", mode_str),
            ).await;
        } else {
            return send_markdown_message(
                &bot, msg.chat.id,
                "Unknown mode. Use `/mode steer` or `/mode queue`.",
            ).await;
        }
    }
```

- [ ] **Step 3: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 4: Commit**

```bash
git add src/platform/telegram.rs
git commit -m "feat(steer): add /mode command to toggle between steer and queue modes"
```

---

### Task 8: Move injection point to pre-LLM-call and add steer formatting

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Remove the old injection drain from pre-iteration**

Find the current injection drain at the start of the main agentic iteration loop (around line 757):
```rust
// CHECK: pending injections from user?
let injections = self.drain_injections(user_id).await;
for text in &injections {
    let inject_msg = ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::from_text(format!(
            "**[User injected mid-processing]:** {}",
            text
        ))),
        tool_calls: None,
        tool_call_id: None,
    };
    // Save to persistent memory
    if let Err(e) = self
        .memory
        .save_message(&conversation_id, &inject_msg)
        .await
    {
        warn!("Failed to persist injected message: {}", e);
    }
    messages.push(inject_msg);
}
```

Replace it with nothing (remove those lines entirely — the drain moves to the new location).

- [ ] **Step 2: Add the new injection drain before `prepare_messages_for_llm`**

Find `let base_prompt = prepare_messages_for_llm(&messages, context_window);` (line 839).
Just before it, add the new injection drain with mode-aware formatting:

```rust
        // CHECK: pending injections from user (steer or queue based on mode)
        let inject_mode = self.get_mid_run_mode(user_id).await;
        let injections = self.drain_injections(user_id).await;
        if !injections.is_empty() {
            let label = if inject_mode == MidRunMode::Steer {
                "**[Steer]:** "
            } else {
                "**[User injected mid-processing]:** "
            };
            for text in &injections {
                let msg = ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::from_text(format!("{}{}", label, text))),
                    tool_calls: None,
                    tool_call_id: None,
                };
                messages.push(msg);
                if inject_mode == MidRunMode::Queue {
                    if let Err(e) = self
                        .memory
                        .save_message(&conversation_id, &msg)
                        .await
                    {
                        warn!("Failed to persist queued injection: {}", e);
                    }
                }
            }
        }

        // Tiers 1-2: sync compaction
        let base_prompt = prepare_messages_for_llm(&messages, context_window);
```

- [ ] **Step 3: Update `/clear` to also reset MidRunMode**

In `clear_conversation` (line 1872), add:
```rust
    pub async fn clear_conversation(&self, platform: &str, user_id: &str) -> Result<()> {
        self.memory.clear_conversation(platform, user_id).await?;
        // Reset mid-run mode to default (Steer)
        self.delete_mid_run_mode(user_id).await;
        Ok(())
    }
```

- [ ] **Step 4: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 5: Update the injection confirmation message in telegram.rs**

In the injection check (line 992-1010), replace the confirmation messages to be mode-aware:

```rust
    // CHECK: if user is currently being processed, queue non-command messages as injection
    if !text.starts_with('/') && agent.is_processing(&user_id.to_string()).await {
        let current_mode = agent.get_mid_run_mode(&user_id.to_string()).await;
        let maxed = !agent.queue_injection(&user_id.to_string(), &text).await;
        if maxed {
            return send_markdown_message(
                &bot,
                msg.chat.id,
                "⚠️ **Injection queue full** (max 10). Please wait for current processing to finish.",
            )
            .await;
        }
        let confirm = match current_mode {
            MidRunMode::Steer => "📨 **Steer queued** — will inject into current processing at next step.",
            MidRunMode::Queue => "📨 **Message queued** — will process after current task completes.",
        };
        return send_markdown_message(&bot, msg.chat.id, confirm).await;
    }
```

- [ ] **Step 6: Ensure `MidRunMode` is imported in telegram.rs**

At the top of telegram.rs, add to the existing `use crate::agent::Agent;` or create a new use:
```rust
use crate::agent::{Agent, MidRunMode};
```

- [ ] **Step 7: Build and verify**

Run: `cargo check`
Expected: No errors

- [ ] **Step 8: Commit**

```bash
git add src/agent.rs src/platform/telegram.rs
git commit -m "feat(steer): move injection to pre-LLM-call with MidRunMode-aware formatting and persistence"
```

---

### Task 9: Final verification

**Files:** (no changes)

- [ ] **Step 1: Run full build**

Run: `cargo build`
Expected: Compiles with no errors

- [ ] **Step 2: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: No warnings

- [ ] **Step 3: Run all tests**

Run: `cargo test`
Expected: All tests pass, including all markdown_entities tests

