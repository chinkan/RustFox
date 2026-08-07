# Conversation Compaction Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix RustFox forgetting user requests on compaction — move compaction to per-user-turn, adopt a persisted running summary with a protected "latest intent" tail, token-based 85% trigger, USER.md memory flush, and a defer-don't-truncate fallback (ADR 0003, Q1–Q9).

**Architecture:** `ConversationManager` gains `summary: Option<String>` + `last_flush_turn: Option<usize>`; `compact_messages(ctx)` runs once per user turn from `process_message` (after `add_user_turn`, before the loop). The protected tail (last 2 user turns + active exchange, ≤20% of window, never mid-tool-pair) is selected by a pure `protected_tail_start` fn. Summary layers extend the running summary, are injected as a system message, and persist as `[SUMMARY]` rows (existing convention — `load_messages_with_limit` already loads them first). A flush turn banks durable facts into USER.md (reusing `learning.rs` machinery). On summary failure: defer with a `warn!` log; the emergency mask in `prepare_messages_for_llm` drops oldest non-protected messages only.

**Tech Stack:** Rust (edition 2021), tokio, anyhow, serde, rusqlite (MemoryStore), teloxide. Tests: in-crate `#[cfg(test)]` + `tests/` integration.

---

## File map

| File | Change |
|------|--------|
| `src/agent_prompt.rs` | Add `estimate_tokens` (CJK-aware), `protected_tail_start`; change `COMPACT_TRIGGER_PCT` to 0.85; remove `COMPACT_LADDER`/`compact_fraction`/`ConversationMeta`/`should_auto_compact`/`COMPACT_TURN_GAP`; rework hard-cap branch of `prepare_messages_for_llm` |
| `src/conversation.rs` | `ConversationManager` gains `summary`, `last_flush_turn`; load-time `[SUMMARY]` folding; `CompactionContext`; rewrite `compact_messages` (flush → summarize → apply); add `apply_summary_layer`, `should_flush`; remove `summarize_with_llm` marker prompt, `build_sync_summary`, `MAX_CHARS` |
| `src/learning.rs` | Extract `build_user_model_prompt`, `write_user_model_with_backup`, `write_user_model_from_snippets`; add `flush_user_model` |
| `src/config.rs` | `LearningConfig.compaction_model: Option<String>` |
| `config.example.toml` | Document `compaction_model`; fix stale `user_model_path` comment |
| `src/loop_runner.rs` | Remove per-iteration compaction block + `compaction_enabled`; `LoopConfig` gains `context_window` |
| `src/agent.rs` | Wire per-turn compaction after `add_user_turn`; delete `ConversationMeta` line/import; pass real context window to loop |
| `tests/compaction_preserves_user_intent.rs` | New integration regression test |

---

## Task 1: CJK-aware token estimation

**Files:**
- Modify: `src/agent_prompt.rs`
- Test: `src/agent_prompt.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Append to the tests module in `src/agent_prompt.rs`:

```rust
#[test]
fn estimate_tokens_counts_cjk_and_latin() {
    use crate::llm::{ChatMessage, MessageContent};

    fn msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(text.to_string())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    // 4 Latin chars ≈ 1 token.
    let latin = vec![msg("abcd")];
    assert_eq!(estimate_tokens(&latin), 1, "4 latin chars ≈ 1 token");

    // CJK chars cost 1 token each.
    let cjk = vec![msg("中文测试")];
    assert_eq!(estimate_tokens(&cjk), 4, "CJK chars ≈ 1 token each");

    // Mixed.
    let mixed = vec![msg("hello中文")];
    assert_eq!(estimate_tokens(&mixed), 1 + 2, "latin/4 + cjk");

    // Tool-call arguments count toward the total.
    let with_tool = vec![ChatMessage {
        role: "assistant".to_string(),
        content: None,
        tool_calls: Some(vec![crate::llm::ToolCall {
            id: "c1".to_string(),
            call_type: "function".to_string(),
            function: crate::llm::FunctionCall {
                name: "search".to_string(),
                arguments: r#"{"q":"abcd"}"#.to_string(),
            },
        }]),
        tool_call_id: None,
    }];
    assert_eq!(estimate_tokens(&with_tool), 1, "tool args count");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rustfox estimate_tokens_counts_cjk_and_latin --lib`
Expected: FAIL — `estimate_tokens` not found.

- [ ] **Step 3: Write the implementation**

Add to `src/agent_prompt.rs`, next to `estimate_prompt_bytes` (line ~211):

```rust
/// Estimate token count from messages, CJK-aware.
///
/// CJK characters cost ~1 token each; Latin/other characters ~1/4 token.
/// Tool-call arguments count toward the total. This is the single token
/// estimate used for the compaction trigger (ADR 0003 Q3).
pub fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    let mut latin_chars = 0usize;
    let mut cjk_chars = 0usize;
    for msg in messages {
        if let Some(content) = msg.content.as_ref() {
            count_chars(content.as_text(), &mut latin_chars, &mut cjk_chars);
        }
        if let Some(calls) = msg.tool_calls.as_ref() {
            for call in calls {
                count_chars(&call.function.arguments, &mut latin_chars, &mut cjk_chars);
            }
        }
    }
    latin_chars / 4 + cjk_chars
}

fn count_chars(text: &str, latin: &mut usize, cjk: &mut usize) {
    for ch in text.chars() {
        if is_cjk(ch) {
            *cjk += 1;
        } else {
            *latin += 1;
        }
    }
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x2E80..=0x2EFF | // CJK Radicals Supplement
        0x3000..=0x303F | // CJK punctuation
        0x3040..=0x30FF | // Hiragana + Katakana
        0x3400..=0x4DBF | // CJK Extension A
        0x4E00..=0x9FFF | // CJK Unified Ideographs
        0xAC00..=0xD7AF   // Hangul
    )
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rustfox estimate_tokens_counts_cjk_and_latin --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/agent_prompt.rs
git commit -m "feat: CJK-aware token estimation for compaction trigger"
```

---

## Task 2: Protected-tail selection (`protected_tail_start`)

**Files:**
- Modify: `src/agent_prompt.rs`
- Test: `src/agent_prompt.rs`

- [ ] **Step 1: Write the failing test**

Append to the tests module in `src/agent_prompt.rs`:

```rust
fn chat_msg(role: &str, text: &str) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content: Some(MessageContent::Text(text.to_string())),
        tool_calls: None,
        tool_call_id: None,
    }
}

fn tool_call_msg(id: &str, name: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: None,
        tool_calls: Some(vec![crate::llm::ToolCall {
            id: id.to_string(),
            call_type: "function".to_string(),
            function: crate::llm::FunctionCall {
                name: name.to_string(),
                arguments: "{}".to_string(),
            },
        }]),
        tool_call_id: None,
    }
}

fn tool_result_msg(id: &str) -> ChatMessage {
    ChatMessage {
        role: "tool".to_string(),
        content: Some(MessageContent::Text("result payload".to_string())),
        tool_calls: None,
        tool_call_id: Some(id.to_string()),
    }
}

#[test]
fn protected_tail_start_keeps_last_two_user_turns() {
    let mut msgs = vec![chat_msg("system", "sys")];
    for i in 0..10 {
        msgs.push(chat_msg("user", &format!("turn {i}")));
        msgs.push(chat_msg("assistant", &format!("reply {i}")));
    }
    let start = protected_tail_start(&msgs, 1_000_000);
    let tail: Vec<&str> = msgs[start..].iter().map(|m| m.role.as_str()).collect();
    assert!(tail.iter().any(|r| *r == "user"), "tail keeps user turns");
    assert_eq!(
        msgs[start..]
            .iter()
            .filter(|m| m.role == "user")
            .count(),
        2,
        "exactly the last two user turns survive"
    );
    assert!(
        msgs[start..].iter().any(|m| {
            m.content
                .as_ref()
                .map(|c| c.as_text() == "turn 8")
                .unwrap_or(false)
        }),
        "second-to-last user turn verbatim"
    );
    assert!(
        msgs[start..].iter().any(|m| {
            m.content
                .as_ref()
                .map(|c| c.as_text() == "turn 9")
                .unwrap_or(false)
        }),
        "last user turn verbatim"
    );
}

#[test]
fn protected_tail_start_never_splits_tool_pair() {
    // user, call, result, user — boundary must not land between call and result.
    let msgs = vec![
        chat_msg("system", "sys"),
        chat_msg("user", "old request"),
        tool_call_msg("call_a", "lookup_thing"),
        tool_result_msg("call_a"),
        chat_msg("assistant", "old answer"),
        chat_msg("user", "latest request"),
    ];
    let start = protected_tail_start(&msgs, 1_000_000);
    let tail = &msgs[start..];
    assert!(
        !(tail.iter().any(|m| m.tool_call_id.as_deref() == Some("call_a"))
            && !tail.iter().any(|m| {
                m.has_tool_calls()
                    && m.tool_calls.as_ref().is_some_and(|calls| {
                        calls.iter().any(|c| c.id == "call_a")
                    })
            })),
        "orphaned tool result in tail"
    );
    assert!(
        !(tail.iter().any(|m| {
            m.has_tool_calls()
                && m.tool_calls.as_ref().is_some_and(|calls| {
                    calls.iter().any(|c| c.id == "call_a")
                })
        }) && !tail.iter().any(|m| m.tool_call_id.as_deref() == Some("call_a"))),
        "orphaned tool call in tail"
    );
}

#[test]
fn protected_tail_start_caps_at_20_percent() {
    let mut msgs = vec![chat_msg("system", "sys")];
    for i in 0..8 {
        msgs.push(chat_msg("user", &format!("request {i}")));
        msgs.push(chat_msg("assistant", &"reply ".repeat(500)));
    }
    // window sized so the full tail (~4K chars) exceeds 20% of window tokens
    let window = estimate_tokens(&msgs) * 5 / 2; // tail cap = window/5 < tail tokens
    let start = protected_tail_start(&msgs, window);
    let tail_tokens = estimate_tokens(&msgs[start..]);
    assert!(
        tail_tokens <= window / 5 + estimate_tokens(&msgs[msgs.len() - 2..]),
        "tail must be capped near 20% (plus the mandatory last turn): {tail_tokens} > {}",
        window / 5
    );
    // last user turn always survives the cap
    assert!(
        msgs[start..].iter().any(|m| {
            m.content
                .as_ref()
                .map(|c| c.as_text().starts_with("request 7"))
                .unwrap_or(false)
        }),
        "last user turn must never be dropped by the cap"
    );
}

#[test]
fn protected_tail_start_returns_zero_without_user_messages() {
    let msgs = vec![
        chat_msg("system", "sys"),
        chat_msg("assistant", "a"),
        chat_msg("tool", "t"),
    ];
    assert_eq!(protected_tail_start(&msgs, 1_000_000), 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rustfox protected_tail_start --lib`
Expected: FAIL — `protected_tail_start` not found.

- [ ] **Step 3: Write the implementation**

Add to `src/agent_prompt.rs` after `estimate_tokens`:

```rust
/// First index of the protected tail — messages from this index on are kept
/// verbatim (ADR 0003 Q4): the last two user turns plus the active exchange,
/// capped at 20% of `window` tokens. The boundary never splits a
/// [tool_call, tool_result] pair. Returns 0 when nothing can be protected
/// (no user messages) — callers treat 0 as "do not compact".
pub fn protected_tail_start(messages: &[ChatMessage], window: usize) -> usize {
    let user_idx: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .map(|(i, _)| i)
        .collect();
    if user_idx.is_empty() {
        return 0;
    }
    let last_user = *user_idx.last().expect("non-empty");
    let base = if user_idx.len() >= 2 {
        user_idx[user_idx.len() - 2]
    } else {
        user_idx[0]
    };
    if base == 0 {
        return 0; // would protect the system message — nothing to compact
    }

    let mut start = base;
    let cap_tokens = window / 5;
    while start < last_user && estimate_tokens(&messages[start..]) > cap_tokens {
        start += 1;
    }

    // Never split a [tool_call, tool_result] pair.
    loop {
        let mut changed = false;
        for i in start..messages.len() {
            let msg = &messages[i];
            if msg.has_tool_calls() {
                let call_ids: Vec<&str> = msg
                    .tool_calls
                    .iter()
                    .flatten()
                    .map(|c| c.id.as_str())
                    .collect();
                let mut last_result = i;
                for j in (i + 1)..messages.len() {
                    let m = &messages[j];
                    if m.role == "tool"
                        && m.tool_call_id
                            .as_deref()
                            .is_some_and(|id| call_ids.contains(&id))
                    {
                        last_result = j;
                    } else if m.role != "tool" {
                        break;
                    }
                }
                if last_result + 1 > start {
                    start = last_result + 1;
                    changed = true;
                }
            }
        }
        if start > 0 {
            let prev = &messages[start - 1];
            if prev.role == "tool" {
                let call_in_tail = messages[start..].iter().any(|m| {
                    m.has_tool_calls()
                        && m.tool_calls.as_ref().is_some_and(|calls| {
                            calls
                                .iter()
                                .any(|c| Some(c.id.as_str()) == prev.tool_call_id.as_deref())
                        })
                });
                if call_in_tail {
                    start -= 1;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    start
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rustfox protected_tail_start --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/agent_prompt.rs
git commit -m "feat: protected-tail selection keeps latest user intent (ADR Q4)"
```

---

## Task 3: `ConversationManager` summary state + persistence

**Files:**
- Modify: `src/conversation.rs`
- Test: `src/conversation.rs`

- [ ] **Step 1: Write the failing tests**

Add to the tests module in `src/conversation.rs` (replace the `manager` helper's struct literal to include the new fields):

```rust
fn manager(messages: Vec<ChatMessage>) -> ConversationManager {
    ConversationManager {
        messages,
        system_prompt: String::new(),
        memory: crate::memory::MemoryStore::open_in_memory().unwrap(),
        conversation_id: String::new(),
        summary: None,
        last_flush_turn: None,
    }
}
```

And add these tests:

```rust
#[tokio::test]
async fn should_flush_gate() {
    // no user message in range → never flush
    assert!(!should_flush(None, None));
    // first flush with a user message → yes
    assert!(should_flush(Some(3), None));
    // same range as last flush → no
    assert!(!should_flush(Some(3), Some(3)));
    // newer user message than last flush → yes
    assert!(should_flush(Some(7), Some(3)));
}

#[tokio::test]
async fn apply_summary_layer_rebuilds_messages_and_persists() {
    let store = crate::memory::MemoryStore::open_in_memory().unwrap();
    let conv = store
        .get_or_create_conversation("test", "layer_u1")
        .await
        .unwrap();
    let mut cm = manager(vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text("sys".to_string())),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("old request".to_string())),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "assistant".to_string(),
            content: Some(MessageContent::Text("old reply".to_string())),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text("latest request".to_string())),
            tool_calls: None,
            tool_call_id: None,
        },
    ]);
    cm.conversation_id = conv.clone();

    cm.apply_summary_layer("layer one content", 3).await.unwrap();

    // Rebuilt: system + summary block + tail from index 3.
    assert_eq!(cm.messages.len(), 3);
    assert_eq!(cm.messages[0].role, "system");
    assert_eq!(cm.messages[1].role, "system");
    assert!(
        cm.messages[1].content.as_ref().unwrap().as_text().contains(
            "Previously compacted context:\nlayer one content"
        ),
        "summary injected as system message: {}",
        cm.messages[1].content.as_ref().unwrap().as_text()
    );
    assert_eq!(cm.messages[2].content.as_ref().unwrap().as_text(), "latest request");

    // Second layer extends, not replaces.
    cm.apply_summary_layer("layer two content", 2).await.unwrap();
    let summary_text = cm.messages[1].content.as_ref().unwrap().as_text();
    assert!(
        summary_text.contains("layer one content") && summary_text.contains("layer two content"),
        "layered extension: {summary_text}"
    );
    assert_eq!(cm.summary.as_deref().unwrap(), "layer one content\n\nlayer two content");

    // Persisted: [SUMMARY] rows reload.
    let reloaded = store.load_messages(&conv).await.unwrap();
    let summary_rows: Vec<&str> = reloaded
        .iter()
        .filter_map(|m| {
            m.content
                .as_ref()
                .map(|c| c.as_text())
                .filter(|t| t.starts_with("[SUMMARY]"))
        })
        .collect();
    assert_eq!(summary_rows.len(), 2, "one [SUMMARY] row per layer");
    assert!(summary_rows[0].contains("layer one content"));
    assert!(summary_rows[1].contains("layer two content"));
}

#[tokio::test]
async fn apply_summary_layer_rejects_empty() {
    let mut cm = manager(vec![ChatMessage {
        role: "system".to_string(),
        content: Some(MessageContent::Text("sys".to_string())),
        tool_calls: None,
        tool_call_id: None,
    }]);
    assert!(cm.apply_summary_layer("   ", 1).await.is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rustfox apply_summary_layer --lib` and `cargo test -p rustfox should_flush --lib`
Expected: FAIL — new fields/functions missing.

- [ ] **Step 3: Add the fields to the struct**

In `src/conversation.rs`:

```rust
pub struct ConversationManager {
    messages: Vec<ChatMessage>,
    system_prompt: String,
    memory: MemoryStore,
    conversation_id: String,
    /// Running summary of compacted history (ADR 0003 Q2) — layered,
    /// persisted as `[SUMMARY]` rows (Q8), injected as a system message.
    summary: Option<String>,
    /// Highest message index whose user turn was already flushed to USER.md (Q6).
    last_flush_turn: Option<usize>,
}
```

- [ ] **Step 4: Fold persisted summaries on load**

In `ConversationManager::new`, replace the history handling (currently lines ~28-31 and the `Ok(Self { ... })` at lines ~52-57):

```rust
        let history = memory
            .load_messages(&conversation_id)
            .await
            .unwrap_or_default();

        let mut folded_summary: Vec<String> = Vec::new();
        let mut raw: Vec<ChatMessage> = Vec::new();
        for m in history {
            if m.role == "system" {
                if let Some(text) = m.content.as_ref().map(|c| c.as_text()) {
                    if let Some(rest) = text.strip_prefix("[SUMMARY]") {
                        folded_summary.push(rest.trim().to_string());
                        continue;
                    }
                }
            }
            if m.role == "user" && m.tool_call_id.as_deref() == Some("summary") {
                continue; // legacy marker-style summary entries are superseded
            }
            raw.push(m);
        }
        let summary = (!folded_summary.is_empty()).then(|| folded_summary.join("\n\n"));
```

Then replace the message assembly (currently lines ~49-57):

```rust
        let mut messages = vec![system_msg];
        if let Some(s) = &summary {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text(format!(
                    "Previously compacted context:\n{s}"
                ))),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        messages.extend(raw);

        Ok(Self {
            messages,
            system_prompt,
            memory: memory.clone(),
            conversation_id,
            summary,
            last_flush_turn: None,
        })
```

- [ ] **Step 5: Add `should_flush` and `apply_summary_layer`**

Add after `add_user_turn` in `src/conversation.rs`:

```rust
    /// ADR 0003 Q6: flush only when the range contains a user-authored
    /// message newer than the last flushed one.
    pub(crate) fn should_flush(
        range_user_max: Option<usize>,
        last_flush_turn: Option<usize>,
    ) -> bool {
        match (range_user_max, last_flush_turn) {
            (Some(max), Some(last)) => max > last,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    /// Apply a new summary layer (ADR 0003 Q2/Q8): fold into the running
    /// summary, rebuild the message list as [system, summary block,
    /// protected tail], and persist the layer as a `[SUMMARY]` system
    /// message. Persistence failures are logged and ignored — the in-memory
    /// state wins.
    pub(crate) async fn apply_summary_layer(
        &mut self,
        layer: &str,
        tail_start: usize,
    ) -> Result<()> {
        let layer = layer.trim();
        if layer.is_empty() {
            anyhow::bail!("empty summary layer");
        }
        self.summary = Some(match self.summary.take() {
            Some(prev) => format!("{prev}\n\n{layer}"),
            None => layer.to_string(),
        });

        let mut new_msgs = Vec::with_capacity(2 + self.messages.len().saturating_sub(tail_start));
        if let Some(system) = self.messages.first().cloned() {
            new_msgs.push(system);
        }
        new_msgs.push(ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(format!(
                "Previously compacted context:\n{}",
                self.summary.as_deref().unwrap_or_default()
            ))),
            tool_calls: None,
            tool_call_id: None,
        });
        new_msgs.extend(self.messages.iter().skip(tail_start).cloned());
        self.messages = new_msgs;

        let persisted = ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(format!("[SUMMARY]\n{layer}"))),
            tool_calls: None,
            tool_call_id: None,
        };
        if let Err(e) = self
            .memory
            .save_message(&self.conversation_id, &persisted)
            .await
        {
            tracing::warn!(error = %format!("{e:#}"), "Failed to persist summary layer");
        }
        Ok(())
    }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p rustfox apply_summary_layer --lib && cargo test -p rustfox should_flush --lib`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/conversation.rs
git commit -m "feat: running summary state + [SUMMARY] persistence (ADR Q2/Q8)"
```

---

## Task 4: Rewrite `compact_messages` (per-turn, defer-on-failure)

**Files:**
- Modify: `src/conversation.rs`
- Test: `src/conversation.rs`

- [ ] **Step 1: Write the failing test**

Replace the `compact_messages_sync_fallback_produces_markers` test with:

```rust
#[tokio::test]
async fn compact_messages_defers_on_llm_failure_never_truncates() {
    use crate::agent_prompt::{estimate_tokens, protected_tail_start};

    let mut messages = vec![ChatMessage {
        role: "system".to_string(),
        content: Some(MessageContent::Text("system prompt".to_string())),
        tool_calls: None,
        tool_call_id: None,
    }];
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::Text(format!(
            "UNIQUE_KEYWORD_A long initial request {}",
            "x".repeat(900)
        ))),
        tool_calls: None,
        tool_call_id: None,
    });
    for i in 0..15 {
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![crate::llm::ToolCall {
                id: format!("call_{i}"),
                call_type: "function".to_string(),
                function: crate::llm::FunctionCall {
                    name: "search".to_string(),
                    arguments: format!(r#"{{"q":"{}"}}"#, "y".repeat(120)),
                },
            }]),
            tool_call_id: None,
        });
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text(format!(
                "tool result {}",
                "z".repeat(200)
            ))),
            tool_calls: None,
            tool_call_id: Some(format!("call_{i}")),
        });
    }
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::Text(
            "UNIQUE_KEYWORD_B follow-up request".to_string(),
        )),
        tool_calls: None,
        tool_call_id: None,
    });

    let mut cm = manager(messages);
    let llm = failing_llm();
    let original_len = cm.messages.len();
    let window = estimate_tokens(&cm.messages);
    assert!(window > 0);

    let ctx = CompactionContext {
        llm: &llm,
        context_window: window,
        compaction_model: None,
        user_model_path: None,
    };
    let result = cm.compact_messages(&ctx).await.unwrap();

    // LLM failure → defer: no compaction, no truncation, nothing lost.
    assert!(!result, "must defer when summarization fails");
    assert_eq!(cm.messages.len(), original_len, "messages unchanged");

    let texts: Vec<String> = cm
        .messages
        .iter()
        .map(|m| m.content.as_ref().map(|c| c.as_text()).unwrap_or_default())
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("UNIQUE_KEYWORD_A")),
        "initial request preserved verbatim"
    );
    assert!(
        texts.last().unwrap().contains("UNIQUE_KEYWORD_B"),
        "latest user intent preserved verbatim"
    );
    assert!(
        texts
            .iter()
            .all(|t| t.len() >= 200 || !t.contains("UNIQUE_KEYWORD_A")),
        "no 200-char truncation anywhere"
    );

    // Second attempt: protected tail must include both user turns.
    let tail = protected_tail_start(&cm.messages, window);
    assert!(
        cm.messages[tail..]
            .iter()
            .any(|m| m.content.as_ref().map(|c| c.as_text()).is_some_and(|t| t.contains("UNIQUE_KEYWORD_B"))),
        "protected tail contains the latest user turn"
    );
}
```

And add a success-path test that injects the layer directly (LLM-independent):

```rust
#[tokio::test]
async fn compact_success_path_preserves_user_intent() {
    let store = crate::memory::MemoryStore::open_in_memory().unwrap();
    let conv = store
        .get_or_create_conversation("test", "compact_u1")
        .await
        .unwrap();
    let mut cm = manager(vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text("sys".to_string())),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(format!(
                "UNIQUE_KEYWORD_A old request {}",
                "x".repeat(800)
            ))),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "assistant".to_string(),
            content: Some(MessageContent::Text("old reply".to_string())),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(
                "UNIQUE_KEYWORD_B follow-up".to_string(),
            )),
            tool_calls: None,
            tool_call_id: None,
        },
    ]);
    cm.conversation_id = conv.clone();

    let tail = crate::agent_prompt::protected_tail_start(&cm.messages, 1_000_000);
    assert_eq!(tail, 3, "old request + reply summarized, follow-up protected");
    cm.apply_summary_layer("user asked about UNIQUE_KEYWORD_A topic", tail)
        .await
        .unwrap();

    // System message at index 1 carries the summary; the latest intent is verbatim.
    assert_eq!(cm.messages[1].role, "system");
    let summary_text = cm.messages[1].content.as_ref().unwrap().as_text();
    assert!(
        summary_text.contains("UNIQUE_KEYWORD_A"),
        "summary preserves the old intent: {summary_text}"
    );
    assert_eq!(
        cm.messages[2].content.as_ref().unwrap().as_text(),
        "UNIQUE_KEYWORD_B follow-up"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rustfox compact_ --lib`
Expected: FAIL — `CompactionContext` not found; `compact_messages` signature mismatch.

- [ ] **Step 3: Add `CompactionContext`**

Add near the top of `src/conversation.rs`:

```rust
/// Inputs for one compaction pass (ADR 0003).
pub struct CompactionContext<'a> {
    pub llm: &'a LlmClient,
    /// Provider window in tokens (from `registry.effective_context_window`).
    pub context_window: usize,
    /// Optional cheaper model for summary + flush turns (Q9).
    pub compaction_model: Option<&'a str>,
    /// USER.md path for the durable-memory flush (Q5); `None` disables flush.
    pub user_model_path: Option<&'a std::path::Path>,
}
```

- [ ] **Step 4: Replace `compact_messages` + `summarize_with_llm`, delete `build_sync_summary`**

Replace the whole `compact_messages` body (lines ~127-243), `summarize_with_llm` (lines ~246-295), and delete `build_sync_summary` + its `MAX_CHARS` const + the `HashMap` import if it becomes unused (it is used only by `build_sync_summary`; remove `use std::collections::HashMap;` if `build_sync_summary` was its only user).

```rust
    /// Unified compaction pipeline (ADR 0003 Q1): compress the oldest
    /// messages once total estimated tokens cross 85% of the real provider
    /// window. The protected tail (last two user turns + active exchange,
    /// never mid-tool-pair) stays verbatim. Durable facts are flushed to
    /// USER.md before the running summary is extended. On summarizer
    /// failure the pass is DEFERRED — nothing is truncated (Q7).
    pub async fn compact_messages(&mut self, ctx: &CompactionContext<'_>) -> Result<bool> {
        if ctx.context_window == 0 {
            return Ok(false);
        }
        let trigger_tokens =
            (ctx.context_window as f64 * crate::agent_prompt::COMPACT_TRIGGER_PCT) as usize;
        if crate::agent_prompt::estimate_tokens(&self.messages) <= trigger_tokens {
            return Ok(false);
        }

        let tail_start =
            crate::agent_prompt::protected_tail_start(&self.messages, ctx.context_window);
        if tail_start == 0 || tail_start >= self.messages.len() {
            return Ok(false);
        }
        let range: Vec<&ChatMessage> = self
            .messages
            .iter()
            .skip(1)
            .take(tail_start - 1)
            .collect();
        if range.is_empty() {
            return Ok(false);
        }

        // Q5/Q6: durable-memory flush before the summary is written.
        let range_user_max = range
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == "user")
            .map(|(i, _)| i + 1) // range index 0 == message index 1
            .max();
        if Self::should_flush(range_user_max, self.last_flush_turn) {
            if let Some(path) = ctx.user_model_path {
                match crate::learning::flush_user_model(ctx.llm, path, &range, ctx.compaction_model)
                    .await
                {
                    Ok(true) => {
                        self.last_flush_turn = range_user_max;
                    }
                    Ok(false) => tracing::info!("User-model flush skipped: no durable facts"),
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "User-model flush failed");
                    }
                }
            }
        }

        // Q2/Q7: extend the running summary; defer on failure.
        let layer = match self.summarize_with_llm(ctx, &range).await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    range = range.len(),
                    "Compaction summary failed; deferring (no truncation)"
                );
                return Ok(false);
            }
        };

        self.apply_summary_layer(&layer, tail_start).await?;
        Ok(true)
    }

    /// Ask the summarizer (Q9 model override, else current model) to EXTEND
    /// the running summary with the new portion of the conversation.
    async fn summarize_with_llm(
        &self,
        ctx: &CompactionContext<'_>,
        to_summarize: &[&ChatMessage],
    ) -> Result<String> {
        let summary_text: String = to_summarize
            .iter()
            .map(|m| {
                format!(
                    "{}: {}",
                    m.role,
                    m.content.as_ref().map(|c| c.as_text()).unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let previous = self.summary.as_deref().unwrap_or("");
        let summary_prompt = format!(
            "You are maintaining a running summary of a long conversation.\n\
             {prev_block}\
             Below is the new portion of the conversation. EXTEND the previous summary with it:\n\
             - Preserve key facts, decisions, preferences, and open questions\n\
             - Merge new information; never contradict or repeat the previous summary\n\
             - Be concise — at most 300 words\n\
             - Output ONLY the new summary text (no preamble, no markers)\n\n\
             New conversation:\n{summary_text}",
            prev_block = if previous.is_empty() {
                String::new()
            } else {
                format!("Previous summary:\n{previous}\n\n")
            },
        );

        let summary_msg = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text(
                    "You are a conversation summarizer.".to_string(),
                )),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(MessageContent::Text(summary_prompt)),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let response = match ctx.compaction_model {
            Some(model) => ctx
                .llm
                .chat_completion_with_model(&summary_msg, &[], model)
                .await?
                .message,
            None => ctx.llm.chat(&summary_msg, &[]).await?,
        };
        Ok(response
            .content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap_or_default())
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p rustfox compact_ --lib`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/conversation.rs
git commit -m "feat: per-turn compaction with defer-on-failure (ADR Q1/Q7)"
```

---

## Task 5: Remove dead compaction machinery

**Files:**
- Modify: `src/agent_prompt.rs`

- [ ] **Step 1: Change the trigger constant**

In `src/agent_prompt.rs` (line ~106):

```rust
/// Utilization fraction (estimated tokens / context_window) at which the
/// unified compaction pipeline begins summarizing the oldest messages.
pub const COMPACT_TRIGGER_PCT: f64 = 0.85;
```

- [ ] **Step 2: Delete dead items**

Delete all of the following from `src/agent_prompt.rs`:

1. `COMPACT_LADDER` const + `compact_fraction` fn (lines ~107-127).
2. `COMPACT_TURN_GAP` const (line ~134) — only used by `should_auto_compact`.
3. `ConversationMeta` struct + `impl ConversationMeta` + `impl Default` (lines ~162-190).
4. `should_auto_compact` fn (lines ~530-545).
5. The tests `should_auto_compact_checks_bytes_turns_and_recursion_guard`, `should_auto_compact_needs_minimum_message_count`, and `compact_fraction_ladder`.

Keep: `OBSERVATION_MASK_PCT`, `COLLAPSE_PCT`, `COMPACT_PCT` (used by emergency tiers), `COMPACTION_MARKER_PREFIX` (used by `is_compacted_regurgitation` in agent.rs), `estimate_prompt_bytes`, `compact_min_message_count`.

- [ ] **Step 3: Remove the `ConversationMeta` import from agent.rs**

In `src/agent.rs` (line 14):

```rust
use crate::agent_prompt::{PreparedPrompt};
```

- [ ] **Step 4: Verify compile**

Run: `cargo check`
Expected: PASS with no warnings about the removed items (clippy may still flag `COMPACT_PCT` if unused — check next task; it is used by `should_auto_compact` only, so remove `COMPACT_PCT` and `REACTIVE_PCT` too if `cargo check`/`clippy` reports them unused after this task).

- [ ] **Step 5: Run full test suite**

Run: `cargo test`
Expected: PASS (old `compact_fraction_ladder` tests are gone; any remaining references to removed symbols fail loudly — fix by deleting).

- [ ] **Step 6: Commit**

```bash
git add src/agent_prompt.rs src/agent.rs
git commit -m "refactor: remove dead compaction constants and ConversationMeta"
```

---

## Task 6: USER.md flush machinery in `learning.rs`

**Files:**
- Modify: `src/learning.rs`
- Test: `src/learning.rs`

- [ ] **Step 1: Write the failing tests**

Append to the tests module in `src/learning.rs`:

```rust
#[tokio::test]
async fn test_flush_user_model_writes_valid_content() {
    use crate::llm::{ChatMessage, MessageContent};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("USER.md");
    let msg = ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::Text(
            "I prefer replies in Traditional Chinese and short answers.".to_string(),
        )),
        tool_calls: None,
        tool_call_id: None,
    };

    let snippets = format_snippets(&[&msg]);
    assert!(snippets.contains("[user]: I prefer replies"));

    // Frontmatter validation gate.
    assert!(has_valid_frontmatter("---\nname: user-model\n---\n\nbody"));
    assert!(!has_valid_frontmatter("no frontmatter here"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rustfox flush_user_model --lib`
Expected: FAIL — `format_snippets` not found.

- [ ] **Step 3: Add the shared helpers and `flush_user_model`**

In `src/learning.rs`, in the "Feature 3: User Model" section, add:

```rust
/// Shared: format message excerpts for the user-model update prompt.
fn format_snippets(messages: &[&ChatMessage]) -> String {
    messages
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .filter_map(|m| {
            m.content
                .as_ref()
                .map(|c| format!("[{}]: {}", m.role, c.as_text()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Shared: build the user-model update prompt from existing content + snippets.
fn build_user_model_prompt(existing: &str, snippets: &str) -> String {
    format!(
        "You maintain a concise user profile for an AI assistant.\n\
         \n\
         Current user model:\n```\n{existing}\n```\n\
         \n\
         Recent conversation excerpts:\n```\n{snippets}\n```\n\
         \n\
         Update the user model based on the conversations. Rules:\n\
         - Keep the YAML frontmatter exactly as-is (name, description, tags)\n\
         - Update fields: user_name, language, communication_style, preferences, \
           interests, context\n\
         - Be concise — max 500 words total\n\
         - Only add information the user explicitly stated or strongly implied\n\
         - Do not remove existing valid entries — merge new info\n\
         - Output the COMPLETE updated file (frontmatter + body), nothing else"
    )
}

/// Shared: validated write with `.bak` backup before overwrite.
async fn write_user_model_with_backup(user_model_path: &Path, new_content: &str) -> Result<()> {
    if let Some(parent) = user_model_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    if user_model_path.exists() {
        let mut bak_path = user_model_path.to_string_lossy().to_string();
        bak_path.push_str(".bak");
        let _ = tokio::fs::copy(user_model_path, &bak_path).await;
    }
    tokio::fs::write(user_model_path, new_content)
        .await
        .with_context(|| format!("Failed to write user model: {}", user_model_path.display()))?;
    Ok(())
}

/// Shared: prompt → LLM (optionally model-overridden) → frontmatter-validated
/// write. Returns `Ok(false)` when there is nothing to write.
async fn write_user_model_from_snippets(
    llm: &LlmClient,
    user_model_path: &Path,
    snippets: &str,
    model: Option<&str>,
) -> Result<bool> {
    if snippets.trim().is_empty() {
        return Ok(false);
    }
    let existing = if user_model_path.exists() {
        tokio::fs::read_to_string(user_model_path)
            .await
            .unwrap_or_default()
    } else {
        DEFAULT_USER_MODEL.to_string()
    };
    let prompt = build_user_model_prompt(&existing, snippets);
    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::Text(prompt)),
        tool_calls: None,
        tool_call_id: None,
    }];

    let response = match model {
        Some(m) => llm.chat_completion_with_model(&messages, &[], m).await?,
        None => llm.chat(&messages, &[]).await?,
    };
    let new_content = response.content.unwrap_or_default().as_text();

    // Strict validation: must start with `---` and contain a closing `---`
    // delimiter so we don't write malformed or injection-bearing content into
    // USER.md (which is later injected into the system prompt).
    if !has_valid_frontmatter(&new_content) || new_content.trim().is_empty() {
        warn!("User model update returned invalid content, skipping");
        return Ok(false);
    }

    write_user_model_with_backup(user_model_path, &new_content).await?;
    info!("User model updated: {}", user_model_path.display());
    Ok(true)
}

/// Pre-compaction flush (ADR 0003 Q5): bank durable facts from the
/// to-be-summarized range into USER.md so compaction cannot erase them.
pub async fn flush_user_model(
    llm: &LlmClient,
    user_model_path: &Path,
    range: &[&ChatMessage],
    model: Option<&str>,
) -> Result<bool> {
    let snippets = format_snippets(range);
    write_user_model_from_snippets(llm, user_model_path, &snippets, model).await
}
```

- [ ] **Step 4: Refactor `update_user_model_inner` to use the shared helpers**

Replace the body of `update_user_model_inner` (lines ~489-576) with:

```rust
async fn update_user_model_inner(
    llm: &LlmClient,
    memory: &crate::memory::MemoryStore,
    user_model_path: &Path,
) -> Result<bool> {
    // Load recent conversation messages for context.
    let recent = memory
        .search_messages("user preferences interests communication", 20)
        .await
        .unwrap_or_default();

    if recent.len() < MIN_MESSAGES_FOR_USER_MODEL {
        return Ok(false); // Not enough data yet
    }

    let refs: Vec<&ChatMessage> = recent.iter().collect();
    let snippets = format_snippets(&refs);
    write_user_model_from_snippets(llm, user_model_path, &snippets, None).await
}
```

(Delete the old prompt/validation/backup/write code — now in the shared helpers.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p rustfox flush_user_model --lib && cargo test -p rustfox user_model --lib`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/learning.rs
git commit -m "feat: USER.md pre-compaction flush (ADR Q5/Q6)"
```

---

## Task 7: `compaction_model` config key

**Files:**
- Modify: `src/config.rs`
- Modify: `config.example.toml`

- [ ] **Step 1: Add the field**

In `src/config.rs`, `LearningConfig` (after `user_model_cron`, line ~391):

```rust
    /// Optional model override for compaction summary + USER.md flush turns
    /// (ADR 0003 Q9). Empty default = the conversation's current model.
    #[serde(default)]
    pub compaction_model: Option<String>,
```

- [ ] **Step 2: Verify defaults still compile**

Run: `cargo check`
Expected: PASS.

- [ ] **Step 3: Document in `config.example.toml`**

Replace the stale `[learning]` comment block (lines ~249-254) with:

```toml
# ── Self-Learning (optional; defaults apply if section omitted) ─────────────
# [learning]
# skill_extraction_enabled = true              # Auto-generate skills from tool-heavy tasks
# skill_extraction_threshold = 5              # Min tool calls to trigger extraction
# user_model_update_interval = 10             # Update user model every N user messages
# user_model_cron = "0 0 3 * * SUN"           # Weekly user model refresh (6-field cron)
# compaction_model = "qwen/qwen3-8b"          # Cheaper model for compaction summary + USER.md flush (default: current model)
```

- [ ] **Step 4: Commit**

```bash
git add src/config.rs config.example.toml
git commit -m "feat: compaction_model config override (ADR Q9)"
```

---

## Task 8: Move compaction out of the loop; wire per-turn call

**Files:**
- Modify: `src/loop_runner.rs`
- Modify: `src/agent.rs`

- [ ] **Step 1: Remove the loop compaction block and hardcoded window**

In `src/loop_runner.rs`:

1. Delete lines 108-118 (the `if self.config.compaction_enabled ... compact_messages` block).
2. Replace `let context_window = 128_000;` (line 97) with `let context_window = self.config.context_window;`.
3. In `LoopConfig` (lines 27-39): remove `pub compaction_enabled: bool,`, add `pub context_window: usize,`.

- [ ] **Step 2: Update the agent wiring**

In `src/agent.rs` `process_message`:

1. After `cmgr.add_user_turn(user_msg);` (line 742), insert the per-turn compaction (ADR Q1), replacing the `ConversationMeta` line (745):

```rust
        // Per-turn compaction (ADR 0003 Q1): routine compaction runs once per
        // user turn, before the agentic loop, at 85% of the real provider window.
        let current_model = self.current_model.read().await.clone();
        let context_window = self.registry.effective_context_window(&current_model);
        let compaction_model = self.config.learning.compaction_model.clone();
        let user_model_path = self
            .config
            .resolved_home
            .as_ref()
            .map(|h| h.join("USER.md"));
        let compact_ctx = crate::conversation::CompactionContext {
            llm: &self.llm,
            context_window,
            compaction_model: compaction_model.as_deref(),
            user_model_path: user_model_path.as_deref(),
        };
        if let Err(e) = cmgr.compact_messages(&compact_ctx).await {
            warn!(
                user_id = %user_id,
                error = %format!("{e:#}"),
                "Per-turn compaction failed"
            );
        }
```

2. In `loop_config` (lines ~795-807): remove `compaction_enabled: true,`, add `context_window,`.

- [ ] **Step 3: Verify compile + tests**

Run: `cargo check && cargo test -p rustfox compact_ --lib`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/loop_runner.rs src/agent.rs
git commit -m "feat: per-turn compaction wired in process_message (ADR Q1)"
```

---

## Task 9: Emergency mask protects latest intent (Q7 step 3)

**Files:**
- Modify: `src/agent_prompt.rs`
- Test: `src/agent_prompt.rs`

- [ ] **Step 1: Write the failing test**

Append to tests in `src/agent_prompt.rs`:

```rust
#[test]
fn hard_cap_fallback_drops_oldest_only_keeps_latest_user_intent() {
    let mut msgs = vec![chat_msg("system", "sys")];
    for i in 0..6 {
        msgs.push(chat_msg("user", &format!("request {i}")));
        msgs.push(chat_msg("assistant", &"reply ".repeat(300)));
    }
    // Force the hard-cap path: obs/coll thresholds on a tiny window fail to
    // reduce below PROMPT_HARD_CAP_BYTES.
    let prepared = prepare_messages_for_llm(&msgs, 1_000);
    assert!(
        prepared
            .messages
            .iter()
            .any(|m| m.content.as_ref().map(|c| c.as_text() == "request 5").unwrap_or(false)),
        "last user turn survives the hard cap"
    );
    assert!(
        !prepared
            .messages
            .iter()
            .any(|m| m.content.as_ref().map(|c| c.as_text() == "request 0").unwrap_or(false)),
        "oldest traffic dropped"
    );
}
```

Note: this test uses the `chat_msg` helper added in Task 2.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rustfox hard_cap_fallback --lib`
Expected: FAIL (current code keeps only sys/user + 2 newest, so `request 0` still present).

- [ ] **Step 3: Rework the hard-cap branch**

In `prepare_messages_for_llm` (lines ~470-496), replace the hard-cap branch:

```rust
        // Safety net (ADR 0003 Q7 step 3): if still over the hard cap after
        // Tiers 1-2, drop the OLDEST non-protected traffic only — the last two
        // user turns + active exchange always survive.
        if estimate_prompt_bytes(&after_tier2) > PROMPT_HARD_CAP_BYTES {
            let tail_start = protected_tail_start(&after_tier2, context_window).max(1);
            let mut hard_cap_messages: Vec<ChatMessage> = Vec::with_capacity(
                after_tier2.len().saturating_sub(tail_start) + 2,
            );
            if let Some(system) = after_tier2.first() {
                hard_cap_messages.push(system.clone());
            }
            if tail_start < after_tier2.len() {
                if tail_start > 1 {
                    hard_cap_messages.push(ChatMessage {
                        role: "system".to_string(),
                        content: Some(MessageContent::Text(
                            "★ earlier conversation dropped — memory compaction failed ★"
                                .to_string(),
                        )),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                hard_cap_messages.extend(after_tier2.iter().skip(tail_start).cloned());
            }
            hard_cap_messages
        } else {
            after_tier2
        }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rustfox hard_cap_fallback --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/agent_prompt.rs
git commit -m "feat: emergency mask drops oldest traffic only (ADR Q7)"
```

---

## Task 10: Fix existing tests broken by the redesign

**Files:**
- Modify: `src/conversation.rs`
- Modify: `src/agent_prompt.rs`

- [ ] **Step 1: Update `compact_messages_never_splits_tool_pair`**

Replace the body of the existing test with the new API (it currently calls `cm.compact_messages(&llm, 1_000)` with the failing LLM, which now defers and returns `false`). The pair-splitting property now lives in `protected_tail_start` (covered in Task 2); keep this test as a boundary check:

```rust
#[test]
fn compact_range_boundary_lands_after_tool_pair() {
    use crate::agent_prompt::protected_tail_start;
    use crate::llm::{FunctionCall, ToolCall};

    let mut messages = vec![ChatMessage {
        role: "system".to_string(),
        content: Some(MessageContent::Text("system prompt".to_string())),
        tool_calls: None,
        tool_call_id: None,
    }];
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::Text(format!(
            "first request {}",
            "x".repeat(100)
        ))),
        tool_calls: None,
        tool_call_id: None,
    });
    messages.push(ChatMessage {
        role: "assistant".to_string(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "call_split".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "lookup_thing".to_string(),
                arguments: r#"{"query":"x"}"#.to_string(),
            },
        }]),
        tool_call_id: None,
    });
    messages.push(ChatMessage {
        role: "tool".to_string(),
        content: Some(MessageContent::Text("lookup result payload".to_string())),
        tool_calls: None,
        tool_call_id: Some("call_split".to_string()),
    });
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::Text(format!(
            "second request {}",
            "x".repeat(100)
        ))),
        tool_calls: None,
        tool_call_id: None,
    });

    let start = protected_tail_start(&messages, 1_000_000);
    // Boundary must not orphan the pair: both call and result are either
    // both in the tail or both summarized.
    let call_in_tail = messages[start..].iter().any(|m| {
        m.has_tool_calls()
            && m.tool_calls.as_ref().is_some_and(|calls| {
                calls.iter().any(|c| c.id == "call_split")
            })
    });
    let result_in_tail = messages[start..]
        .iter()
        .any(|m| m.tool_call_id.as_deref() == Some("call_split"));
    assert_eq!(
        call_in_tail, result_in_tail,
        "tool pair must not be split at the boundary (start={start})"
    );
}
```

- [ ] **Step 2: Run the conversation + prompt test suites**

Run: `cargo test -p rustfox --lib`
Expected: PASS. Fix any stragglers by deleting tests that assert removed behavior (e.g. any remaining references to `compact_fraction` or `ConversationMeta` in `src/agent_prompt.rs` tests — remove them; the `estimate_prompt_bytes_counts_content_and_tool_arguments` test stays).

- [ ] **Step 3: Commit**

```bash
git add src/conversation.rs src/agent_prompt.rs
git commit -m "test: adapt compaction tests to new API"
```

---

## Task 11: Integration regression test

**Files:**
- Create: `tests/compaction_preserves_user_intent.rs`

- [ ] **Step 1: Write the test**

```rust
//! Regression test: compaction must never lose the user's request, even when
//! the summarizer fails (ADR 0003 Q7). Uses an in-memory store and an LLM
//! client whose provider always fails (empty base_url → relative URL → no
//! network traffic).

use std::collections::HashMap;
use std::sync::Arc;

use rustfox::config::ProviderType;
use rustfox::conversation::{CompactionContext, ConversationManager};
use rustfox::llm::{ChatMessage, LlmClient, MessageContent};
use rustfox::memory::MemoryStore;
use rustfox::provider::{OpenRouterProvider, ProviderConfig, ProviderRegistry};

fn failing_llm() -> LlmClient {
    let config = ProviderConfig {
        name: "test".to_string(),
        provider_type: ProviderType::OpenRouter,
        base_url: String::new(),
        api_key: None,
        default_model: "test-model".to_string(),
        supports_vision: false,
        max_tokens: 100,
        discover_models: false,
        context_window: 4096,
        context_window_cache: Arc::new(tokio::sync::RwLock::new(None)),
        parse_retry_limit: 0,
    };
    let provider: Arc<dyn rustfox::provider::Provider> =
        Arc::new(OpenRouterProvider::new(config));
    let mut providers = HashMap::new();
    providers.insert("test".to_string(), provider);
    LlmClient::new(Arc::new(ProviderRegistry::new(
        providers,
        "test".to_string(),
    )))
}

fn msg(role: &str, text: &str) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content: Some(MessageContent::from_text(text.to_string())),
        tool_calls: None,
        tool_call_id: None,
    }
}

#[tokio::test]
async fn compaction_never_loses_user_request() {
    let store = MemoryStore::open_in_memory().unwrap();
    let conv = store
        .get_or_create_conversation("telegram", "intent_u1")
        .await
        .unwrap();

    // Seed history: long initial request A + 15 tool exchanges + follow-up B.
    let mut history: Vec<ChatMessage> = vec![msg(
        "user",
        &format!("UNIQUE_KEYWORD_A initial request {}", "x".repeat(900)),
    )];
    for i in 0..15 {
        history.push(ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![rustfox::llm::ToolCall {
                id: format!("call_{i}"),
                call_type: "function".to_string(),
                function: rustfox::llm::FunctionCall {
                    name: "search".to_string(),
                    arguments: format!(r#"{{"q":"{}"}}"#, "y".repeat(120)),
                },
            }]),
            tool_call_id: None,
        });
        history.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::from_text(format!(
                "tool result {}",
                "z".repeat(200)
            ))),
            tool_calls: None,
            tool_call_id: Some(format!("call_{i}")),
        });
    }
    for m in &history {
        store.save_message(&conv, m).await.unwrap();
    }

    // Load via the real conversation path, then add the live follow-up.
    let mut cmgr = ConversationManager::new(
        &store,
        "telegram",
        "intent_u1",
        "system prompt".to_string(),
        &rustfox::skills::SkillRegistry::new(),
        &minimal_config(),
    )
    .await
    .unwrap();
    cmgr.add_user_turn(msg("user", "UNIQUE_KEYWORD_B follow-up request"));
    let original_len = cmgr.messages().len();

    let llm = failing_llm();
    let window = rustfox::agent_prompt::estimate_tokens(cmgr.messages());
    let ctx = CompactionContext {
        llm: &llm,
        context_window: window,
        compaction_model: None,
        user_model_path: None,
    };

    // Two passes: both must defer (LLM failure), never truncate.
    for pass in 0..2 {
        let compacted = cmgr.compact_messages(&ctx).await.unwrap();
        assert!(!compacted, "pass {pass}: must defer on summarizer failure");
        assert_eq!(
            cmgr.messages().len(),
            original_len,
            "pass {pass}: messages unchanged"
        );
    }

    let texts: Vec<String> = cmgr
        .messages()
        .iter()
        .map(|m| m.content.as_ref().map(|c| c.as_text()).unwrap_or_default())
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("UNIQUE_KEYWORD_A")),
        "initial request preserved verbatim"
    );
    assert!(
        texts.last().unwrap().contains("UNIQUE_KEYWORD_B"),
        "latest user intent preserved verbatim and last"
    );
}

fn minimal_config() -> rustfox::config::Config {
    // into_path(): the temp dir must outlive the loaded config file.
    let dir = tempfile::tempdir().unwrap().into_path();
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        r#"
[telegram]
bot_token = "test"
allowed_user_ids = [1]

[openrouter]
api_key = "test"

[sandbox]
allowed_directory = "."
"#,
    )
    .unwrap();
    rustfox::config::Config::load(&path).unwrap()
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test --test compaction_preserves_user_intent`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/compaction_preserves_user_intent.rs
git commit -m "test: integration regression — compaction never loses user intent"
```

---

## Task 12: Full verification

- [ ] **Step 1: Format**

Run: `cargo fmt --all -- --check`
Expected: clean. If not: `cargo fmt` and re-check.

- [ ] **Step 2: Lint**

Run: `cargo clippy -- -D warnings`
Expected: clean. Fix any warnings (this is where leftover unused constants like `COMPACT_PCT` surface — delete them).

- [ ] **Step 3: Full test suite**

Run: `cargo test`
Expected: all pass (including pre-existing supervisor/langsmith tests).

- [ ] **Step 4: Update the ADR status**

In `docs/adr/0003-conversation-compaction-redesign.md`, change the Status line to:

```markdown
## Status
Accepted (implemented)
```

- [ ] **Step 5: Commit**

```bash
git add docs/adr/0003-conversation-compaction-redesign.md
git commit -m "docs: ADR 0003 accepted — compaction redesign implemented"
```

---

## Self-review notes

- **Spec coverage:** Q1 → Task 8; Q2/Q8 → Tasks 3+4; Q3 → Tasks 1+5; Q4 → Task 2; Q5/Q6 → Tasks 6+4; Q7 → Tasks 4+9; Q9 → Task 7. All nine ADR decisions have an implementing task.
- **Dependencies:** `cargo check` will fail between Task 3 and Task 4 (old `compact_messages` still calls removed helpers) — if working task-by-task, run Task 4 immediately after Task 3; the `manager()` helper in Task 3 must land with Task 3's struct change or the crate won't compile. Tasks 1-2 are standalone.
- **Manual verification required** (no mock LLM infra): the LLM success path of `compact_messages` is covered by construction (the layer prompt is unit-tested via `apply_summary_layer`), but the live OpenRouter round-trip needs a manual run: start the bot, let a conversation cross 85% of the window, confirm the reply arrives with the summary block visible in `prepare()` output and a `[SUMMARY]` row in the DB.
