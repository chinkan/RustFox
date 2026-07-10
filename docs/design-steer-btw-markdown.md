# Design: Steer Messages, /btw Parallel, & Markdown Upgrade

## 1. Steer Message System

### Problem

Non-command messages sent while the agent is busy are queued as injections,
drained at the next iteration boundary, formatted as full user turns, and
persisted to DB. This makes them indistinguishable from new tasks — the model
treats "use v2 API instead" as a separate instruction rather than mid-turn
steering context. Also, the injection point (pre-iteration) is slow: the user
must wait for the current tool cycle to finish before their steer is seen.

### Design

#### MidRunMode (per-user, persisted in memory store)

```rust
#[derive(Clone, Copy, PartialEq)]
enum MidRunMode {
    Steer,   // (default) inject into current turn, ephemeral, formatted as steering context
    Queue,   // wait for next turn, persisted, formatted as normal user message
}
```

- Default: `Steer`
- Switch via new command: `/mode queue` or `/mode steer`
- Persisted in memory store per user via `memory.remember("settings", "mid_run_mode_{user_id}", "steer", None)` / `memory.recall("settings", "mid_run_mode_{user_id}")`
- Default when no stored value: `Steer` (enforced in code as `unwrap_or(MidRunMode::Steer)`)
- `/clear` resets to default (Steer): in the existing `/clear` handler (`agent.rs ~line 1874`), add `self.memory.delete("settings", &format!("mid_run_mode_{}", user_id)).await.ok();`

#### /stop remains Break

`/stop` is unchanged — it cancels the current token, discards accumulated
state, and saves what's already been persisted. It is the explicit "break"
action and does not participate in the Steer/Queue toggle.

#### Steer formatting

When `mode == Steer`, the injection message is:

```
**[Steer]:** use v2 API instead
```

When `mode == Queue` (or when injection queue was filled while in Queue mode):

```
**[User injected mid-processing]:** some message
```

The `[Steer]` prefix signals the model that this is a **correction/guidance
for the in-progress turn**, not a new user request. The model should adjust
its current trajectory without starting a new task.

#### Steer is NOT persisted

Steer messages (mode=Steer) are appended to the in-memory messages vector for
the LLM call but are **not saved to the database**. This ensures:
- No artificial turn boundaries for compaction
- No pollution of conversation history with course-corrections
- No wasted tokens on compaction of steer messages

Queue messages (mode=Queue) are persisted as they are today (saved to DB).

#### Injection point: pre-LLM-call instead of pre-iteration

Current: drain before tool execution loop (line ~757)
New: drain before `prepare_messages_for_llm()` inside the retry loop, so
injections are visible to every LLM call attempt.

This makes steer delivery responsive — the user's correction is visible to the
model at the very next LLM call, not after the current tool loop finishes.

```rust
// Inside retry loop, before prepare_messages_for_llm():
if cancel_token.is_cancelled() { break; }

// Drain steer/queue injections into messages vector
let inject_mode = self.get_mid_run_mode(user_id).await;
let injections = self.drain_injections(user_id).await;
if !injections.is_empty() {
    let label = if inject_mode == MidRunMode::Steer { "[Steer]" } else { "[User injected mid-processing]" };
    for text in &injections {
        let msg = ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::from_text(format!("**{label}:** {text}"))),
            tool_calls: None,
            tool_call_id: None,
        };
        messages.push(msg);
        if inject_mode == MidRunMode::Queue {
            self.memory.save_message(&conversation_id, &msg).await.ok();
        }
    }
}

// Re-clone from messages (now includes injections) before LLM call
base_prompt = prepare_messages_for_llm(&messages, &conv_meta, context_window)?;
let response = self.llm.chat_completion_with_model(&base_prompt.messages, &all_tools, &model).await;
```

#### Compaction awareness

Steer messages are never persisted so they never reach compaction. Queue-mode
injections are persisted and compacted normally as user messages.

### Commands

| Command | Action |
|---------|--------|
| `/mode` | Show current mode (e.g. "Current mode: **steer**") |
| `/mode steer` | Switch to steer mode (default) |
| `/mode queue` | Switch to queue mode |
| `/stop` | Break — cancel current processing (unchanged) |

Register `/mode` in `supported_commands()` (`telegram.rs:78`) alongside existing commands.

#### Steer vs Queue confirmation messages

When a non-command message is queued during processing (`telegram.rs:999`), the
confirmation text changes based on mode:
- Steer mode: `📨 **Steer queued** — will inject into current processing at next step.`
- Queue mode: `📨 **Message queued** — will process after current task completes.`

## 2. /btw True Parallel Processing

### Problem

`ask_parallel()` calls `run_subagent(None, ...)` which spins up a full
agentic loop (up to max_iterations) with tool access. This is slow, expensive
in tokens, and the tool access is unnecessary — `/btw` should be a quick
read-only knowledge question. Additionally, the function accesses `self.memory`
and other shared resources, creating lock contention.

### Design

Replace with `ask_parallel_lightweight`:

```rust
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
            content: Some(MessageContent::from_text(question)),
            tool_calls: None,
            tool_call_id: None,
        },
    ];
    let response = self.llm.chat(&messages, &[]).await?;
    Ok(response.content.as_ref().map(|c| c.as_text()).unwrap_or_default())
}
```

Properties:
- **Single LLM call** — no agentic loop, no tool execution
- **No tool access** — empty tool list `&[]`
- **No memory/DB access** — zero lock contention with main process
- **No cache write** — ephemeral, won't pollute prompt cache
- **Uses same `self.llm`** — `reqwest::Client` is `Arc` internally, safe for concurrent HTTP
- **Ephemeral output** — answer is sent via Telegram but NOT saved to conversation history

### /btw while main process is NOT processing

If the agent is idle (no active `process_message` for this user), `/btw` still
works identically — it's a light side question that returns an answer without
touching conversation history.

### Remove tool access from /btw

Current `ask_parallel` grants `[read_file, write_file, list_files, execute_command]` tools (via `subagents.default_tools`). `/btw` should have **zero tools** — it's a pure knowledge question from the model's training data + conversation context.

## 3. Markdown Entities Upgrade

### Problem audit

Current `markdown_to_entities()` in `markdown_entities.rs`:

| Feature | Status | Issue |
|---------|-------|-------|
| `**bold**` | ✅ Bold entity | |
| `*italic*` | ✅ Italic entity | |
| `` `code` `` | ✅ Code entity | |
| ` ```rust...``` ` | ✅ Pre { language } | |
| `[text](url)` | ✅ TextLink | |
| `# Heading` | ✅ Bold entity | Low-fi but works |
| `~~strikethrough~~` | ✅ Strikethrough | |
| `> blockquote` | ❌ Text-only `> ` prefix | No Blockquote entity |
| `||spoiler||` | ❌ Not parsed at all | Raw text shown |
| `<u>underline</u>` | ❌ Not parsed | pulldown-cmark ignores raw HTML |
| `- list item` | ⚠️ Text only | No bullet, no indent |
| `1. ordered` | ⚠️ Text only | No numbering entity |
| Tables | ⚠️ Text `|` sep | No Table entity (Telegram doesn't have one) |
| Nested bold+italic | ⚠️ Stack handles nesting | Could overlap incorrectly |

### Upgrades

#### Blockquote entity (Bot API 7.0+)

```diff
+ // Track blockquote start for entity emission
+ let mut blockquote_start: Option<usize> = None;
...
  Tag::BlockQuote(_) => {
      in_blockquote = true;
+     blockquote_start = Some(plain_utf16_len);
  }
...
  TagEnd::BlockQuote(_) => {
      in_blockquote = false;
+     // Emit Blockquote entity (Bot API 7.0+); REMOVE old `> ` text prefix
+     if let Some(start) = blockquote_start.take() {
+         let length = plain_utf16_len.saturating_sub(start);
+         if length > 0 {
+             // MessageEntity is a pub-fields struct (no convenience constructor for Blockquote)
+             entities.push(MessageEntity {
+                 kind: MessageEntityKind::Blockquote,
+                 offset: start,
+                 length,
+             });
+         }
+     }
  }
```

Key change: **remove the `> ` prefix from `Event::Text`** in the `in_blockquote` branch
(currently `markdown_entities.rs:63-68`). Replace it with plain text output — the
Blockquote entity handles the visual formatting. This avoids double-rendering.

Update existing test `test_blockquote_prefixes_with_gt` to assert the text contains
the content **without** `> ` prefix and assert a Blockquote entity is present instead.

Note: The struct literal requires `use teloxide::types::MessageEntityKind;` in scope.

#### Spoiler (`||text||`) & Underline (`<u>text</u>`)

pulldown-cmark does not parse `||spoiler||` or `<u>underline</u>`. Solution:
**single pre-processing pass** with PUA sentinel replacement + post-scan for entities:

```rust
// Sentinel chars (Private Use Area — guaranteed absent from real Markdown, valid UTF-8)
const SPOILER_START: char = '\u{E000}'; // followed by 'S'
const SPOILER_END: char = '\u{E001}';   // followed by "/s"
const UL_START: char = '\u{E002}';      // followed by 'U'
const UL_END: char = '\u{E003}';        // followed by "/u"

/// Pre-process markdown before pulldown-cmark parsing:
/// 1. `<u>text</u>` → `\u{E002}Utext\u{E003}/u`
/// 2. `||text||` → `\u{E000}Stext\u{E001}/s`
fn preprocess_markdown(md: &str) -> String {
    let md = md
        .replace("<u>", format!("{}U", UL_START).as_str())
        .replace("</u>", format!("{}/u", UL_END).as_str());
    let re = regex::Regex::new(r"\|\|(.*?)\|\|").unwrap();
    re.replace_all(&md, format!("{}S$1{}/s", SPOILER_START, SPOILER_END).as_str())
        .to_string()
}
```

After pulldown-cmark parsing, scan the plain text for sentinel markers, remove
them, and emit corresponding entities at correct UTF-16 offsets:

```rust
/// Post-process: remove sentinel markers from plain text, emit spoiler & underline entities.
/// Requires `use teloxide::types::MessageEntityKind;` in scope.
fn postprocess_entities(plain: &mut String, entities: &mut Vec<MessageEntity>) {
    let mut utf16_offset = 0usize;
    let mut out = String::new();
    let mut stack: Vec<(MessageEntityKind, usize)> = Vec::new(); // (kind, utf16_start)
    let chars: Vec<char> = plain.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            c if c == SPOILER_START && i+1 < chars.len() && chars[i+1] == 'S' => {
                stack.push((MessageEntityKind::Spoiler, utf16_offset));
                i += 2; continue;
            }
            c if c == SPOILER_END && i+2 < chars.len() && chars[i+1] == '/' && chars[i+2] == 's' => {
                if let Some(idx) = stack.iter().rposition(|(k,_)| *k == MessageEntityKind::Spoiler) {
                    if let Some((_, start)) = Some(stack.remove(idx)) {
                        let len = utf16_offset - start;
                        if len > 0 { entities.push(MessageEntity::spoiler(start, len)); }
                    }
                }
                i += 3; continue;
            }
            c if c == UL_START && i+1 < chars.len() && chars[i+1] == 'U' => {
                stack.push((MessageEntityKind::Underline, utf16_offset));
                i += 2; continue;
            }
            c if c == UL_END && i+2 < chars.len() && chars[i+1] == '/' && chars[i+2] == 'u' => {
                if let Some(idx) = stack.iter().rposition(|(k,_)| *k == MessageEntityKind::Underline) {
                    if let Some((_, start)) = Some(stack.remove(idx)) {
                        let len = utf16_offset - start;
                        if len > 0 { entities.push(MessageEntity::underline(start, len)); }
                    }
                }
                i += 3; continue;
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

`MessageEntity::spoiler(offset, length)` and `MessageEntity::underline(offset, length)`
both exist in teloxide as convenience constructors (verified).

#### List formatting

Telegram has no list entity type. Lists should render as clean text:
- Unordered: `• item1\n• item2` (using `•` bullet character)
- Ordered: `1. item1\n2. item2`

Current pulldown-cmark already renders list items with `\n` separators. The
fix is ensuring the list marker text is clean.

**Implementation:** Add list tracking state + prefix injection in `Event::Text`:

```rust
// Track list state
let mut list_counter: Option<usize> = None;      // None = unordered, Some(n) = ordered
let mut needs_list_prefix = false;                // true before an item's first text

// In Event::Start match:
Tag::List { start } => {
    list_counter = start; // None for unordered, Some(1|start_number)
}
Tag::Item => {
    needs_list_prefix = true;
}

// In Event::Text match (before appending, when needs_list_prefix is true):
if needs_list_prefix {
    let prefix = match list_counter {
        None => "• ",                               // unordered bullet
        Some(ref mut n) => { let p = format!("{}. ", n); *n += 1; p } // ordered
    };
    plain.push_str(&prefix);
    plain_utf16_len += prefix.encode_utf16().count();
    needs_list_prefix = false;
}
// ... then append text as usual

// In Event::End match:
TagEnd::List(_) => {
    list_counter = None;
    needs_list_prefix = false;
}
TagEnd::Item => {
    plain.push('\n');
    plain_utf16_len += 1;
}
```

### Testing

Add tests for each upgraded feature:
- Blockquote entity with correct UTF-16 offset
- Spoiler span with correct UTF-16 offset
- Underline with correct UTF-16 offset
- Unordered list rendering with bullet prefix
- Ordered list rendering with number prefix
- Nested bold + spoiler
- Mixed formatting inside blockquote

## 4. File Manifest

### New files
- None (all changes are edits to existing files)

### Files to modify

| File | Changes |
|------|---------|
| `src/agent.rs` | Add `MidRunMode` enum, `get_mid_run_mode()`, modify injection point, add steer prefix formatting |
| `src/platform/telegram.rs` | Add `/mode` command handler, modify injection callers for steer vs queue, replace `ask_parallel` call |
| `src/utils/markdown_entities.rs` | Add blockquote entity, spoiler detection, underline detection, list rendering fixes |