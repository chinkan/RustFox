# Context-Aware Compaction & Memory Management for RustFox

## Overview

RustFox's compaction system (Tiers 1-4) currently drops the system prompt, sends all old messages verbatim to the summarizer LLM, produces free-form summaries, and relies on a static `context_window` from config. This spec addresses all four gaps with targeted changes (Approach C).

## Files Touched

| File | Changes |
|------|---------|
| `src/provider.rs` | Add `context_window_cache`, `fetch_context_window()` trait method, `effective_context_window()` |
| `src/agent.rs` | System prompt preservation in auto_compact/reactive_compact, RAG-aware compact flow |
| `src/agent_prompt.rs` | Enhanced structured summary prompt |
| `src/memory/rag.rs` | New `retrieve_context_for_compaction()` function |
| `src/memory/conversations.rs` | Optional metadata columns on vec0, configurable RRF weights |
| `src/config.rs` | Add `rrf_k`, `rrf_weight_fts`, `rrf_weight_vec` to memory config |

---

## Section 1: Dynamic Context Window Detection

### Problem

`context_window` is a static field in `config.toml`. When the user switches models via `/model`, the compaction thresholds (20%/60%/85%) don't adjust — they use the original model's window. The current model is already tracked in `current_model: RwLock<String>`.

### Solution

**Runtime context window cache** that overrides the static config value when available, falls back to static.

**`ProviderConfig` changes** (provider.rs:17-30):
```rust
pub struct ProviderConfig {
    // … existing fields …
    pub context_window: usize,                         // static fallback
    pub context_window_cache: Arc<RwLock<Option<usize>>>,  // runtime override (NEW)
}
```

**New trait method** on `Provider`:
```rust
async fn fetch_context_window(
    &self,
    client: &Client,
    model: &str,
) -> Option<usize>;
```

**OpenRouterProvider implementation** (provider.rs:358-383): The existing `list_models()` already calls `GET /api/v1/models` which returns `context_length` per model. Add a new method that parses `context_length`:

```
GET /api/v1/models → JSON array of { id, context_length, ... }
```

Find the entry where `id` matches the current model, extract `context_length`. Return `None` on error or no match.

**OpenAICompatibleProvider**: Tries the same pattern (many OpenAI-compatible providers return model metadata). Returns `None` on failure.

**OllamaProvider**: Returns `None` — Ollama doesn't report context length.

**`ProviderRegistry` new method**:
```rust
pub fn effective_context_window(&self, model: &str) -> usize {
    let (provider, _) = self.resolve_model(model);
    let cached = provider.config().context_window_cache.read();
    cached.unwrap_or(provider.config().context_window)
}
```

**Trigger points for cache update:**

1. **Startup** (`main.rs` after `build_registry`): Spawn a background task that iterates all registered providers, calling `fetch_context_window` for each provider's default model and writing results to `context_window_cache`. No startup delay — the static config value is used for the first LLM call.

   ```rust
   // In main.rs, after registry is built:
   let registry_clone = Arc::clone(&registry);
   tokio::spawn(async move {
       let client = reqwest::Client::new();
       for name in registry_clone.provider_names() {
           if let Some(provider) = registry_clone.get_provider(&name) {
               let model = provider.default_model();
               if let Some(ctx) = provider.fetch_context_window(&client, model).await {
                   // write to provider's context_window_cache (via ProviderConfig)
                   let mut cache = provider.config().context_window_cache.write();
                   *cache = Some(ctx);
               }
           }
       }
   });
   ```

2. **Model switch** (`set_model()` in agent.rs:328): `Agent` already holds `self.registry: Arc<ProviderRegistry>`. After persisting the new model to config, call `self.refresh_context_window_cache()` which resolves the new model through the registry and calls `fetch_context_window`:

   ```rust
   pub async fn refresh_context_window_cache(&self) {
       let model = self.current_model.read().await.clone();
       let (provider, actual_model) = self.registry.resolve_model(&model);
       let client = reqwest::Client::new();
       if let Some(ctx) = provider.fetch_context_window(&client, actual_model).await {
           let mut cache = provider.config().context_window_cache.write();
           *cache = Some(ctx);
       }
   }
   ```

3. **`/model` command handler**: Calls `agent.set_model(new_model).await` which internally calls `refresh_context_window_cache` (integrated into `set_model`).

**Agent loop** (agent.rs:638-643): Replace:
```rust
// BEFORE:
let context_window = provider.config().context_window;

// AFTER:
let context_window = registry.effective_context_window(&model);
```

**Edge cases:**
- API call fails → silently keep static value (log at debug)
- Model not found in API response → keep static value
- Model switches between static and dynamic providers → cache handles each independently
- Race: context_window_cache uses `Arc<RwLock<Option<usize>>>` so writes never block reads

---

## Section 2: System Prompt Preservation

### Problem

`auto_compact_conversation` (agent.rs:1330) includes `messages[0]` (the system message) in `to_summarize`. The summarizer LLM receives it and replaces everything with a summary. After compaction, the system prompt is gone until the next `process_message()` refreshes it in-memory. If the LLM tries a tool call after compaction (within the same agentic loop), it runs without system instructions.

### Solution

**In `auto_compact_conversation`** (agent.rs:1330-1362):

```rust
async fn auto_compact_conversation(...) -> Result<Vec<ChatMessage>> {
    // 1. Separate system messages from the rest
    let mut system_msgs: Vec<ChatMessage> = Vec::new();
    let mut non_system: Vec<ChatMessage> = Vec::new();
    for msg in messages {
        if msg.role == "system" {
            system_msgs.push(msg.clone());
        } else {
            non_system.push(msg.clone());
        }
    }

    // 2. Compute tool groups and summary split on non-system only
    let tool_groups = find_tool_groups(&non_system);
    let preserve_count = PRESERVED_TOOL_GROUPS.min(tool_groups.len());
    let preserved_groups_start = tool_groups.len().saturating_sub(preserve_count);

    if preserved_groups_start == 0 {
        // Not enough groups to compact — return original (including system)
        return Ok(messages.to_vec());
    }

    // summary_end relative to non_system
    let last_summary = &tool_groups[preserved_groups_start - 1];
    let summary_end = *last_summary.tool_result_indices.last()
        .unwrap_or(&last_summary.assistant_idx) + 1;

    let to_summarize = &non_system[..summary_end];
    let preserved = &non_system[summary_end..];

    // 3. Summarize (existing logic)
    let mut compacted = Self::summarize_and_replace(
        llm, to_summarize, preserved, ...
    ).await?;

    // 4. Prepend system messages back
    let mut result = system_msgs;
    result.append(&mut compacted);
    Ok(result)
}
```

**Same pattern in `reactive_compact`** (agent.rs:1369-1391): Currently uses a simple `PRESERVE_COUNT = 4` from the end. Apply the same partition logic.

**Why this is safe:**
- System messages are read-only context (never tool calls)
- The LLM never modifies them, they don't participate in tool groups
- Partitioning them out before compaction preserves them exactly
- On the next `process_message()` iteration, the system prompt is refreshed anyway by `build_system_prompt()`

---

## Section 3: Enhanced Structured Summary Prompt

### Problem

`build_compact_summary_prompt()` (agent_prompt.rs:440-465) produces a free-form narrative. The subsequent LLM must re-read the entire narrative to extract state — defeating some of the benefit of compaction. There's no machine-readable state block.

### Solution

**Replace `build_compact_summary_prompt()`** with a version that requests both a structured YAML state block and brief narrative:

```rust
pub fn build_compact_summary_prompt() -> ChatMessage {
    let prompt_text = vec![
        "You are producing a compact state summary of the conversation below.",
        "",
        "OUTPUT FORMAT:",
        "",
        "## STATE",
        "```yaml",
        "stage: <problem_definition | investigation | implementation | review | complete>",
        "decisions:",
        "  - <decision made>",
        "pending:",
        "  - <still to do>",
        "last_action: <tool call name + brief result>",
        "last_action_result: <what happened>",
        "conversation_phase: <summary of current focus>",
        "```",
        "",
        "## CONTEXT",
        "- <bullet point of key fact, file, error, or finding>",
        "- <bullet point>",
        "- <bullet point>",
        "",
        "CRITICAL RULES:",
        "- State must be precise enough for the LLM to continue without re-reading history",
        "- Include ALL pending items the user explicitly requested",
        "- Include ALL error messages and their resolutions",
        "- Be specific with file paths and tool names",
        "- Do NOT call any tools. Respond with text only.",
    ].join("\n");

    ChatMessage { role: "system", content: Some(MessageContent::Text(prompt_text)), ... }
}
```

**Update `build_compact_boundary_marker()`** (agent_prompt.rs:468):
```
"★ COMPACT SUMMARY — previous {} msgs → YAML state + narrative (%d msgs) ★"
```

**Update `recovery_nudge_for()`** (agent_prompt.rs:105): Add a hint to read the STATE block:
```
"Continue from the [user's request|tool result] above. Read the ## STATE block in the compact summary for context. Either call the next required tool or provide a final answer."
```

---

## Section 4: RAG-Aware Tier 3/4 Compaction

### Problem

`summarize_and_replace` (agent.rs:1395) sends ALL messages in `to_summarize` verbatim to the summarizer LLM. For a conversation with 50+ tool-call groups, this means 15K-30K tokens just to generate a summary — burning tokens and losing precision in the "middle" of the input.

### Solution

**Before calling the summarizer LLM, retrieve relevant context from the database** using the existing hybrid search.

**New function in `src/memory/rag.rs`:**

```rust
/// Retrieve context for compaction summarization.
///
/// Uses the most recent user message (from both to_summarize and preserved
/// ranges) as a search query to find relevant historical context from the
/// conversation. Returns formatted snippets that help the summarizer write
/// a focused summary.
pub async fn retrieve_context_for_compaction(
    store: &MemoryStore,
    to_summarize: &[ChatMessage],
    preserved: &[ChatMessage],
    conversation_id: &str,
    limit: usize,
) -> Result<Option<String>> {
    // Find the most recent user message across both ranges.
    // The most recent user message may be in `preserved` (the last few tool
    // groups) if compaction fires right after the user spoke before any
    // tool calls happened.
    let query = preserved.iter().rev()
        .chain(to_summarize.iter().rev())
        .find(|m| m.role == "user")
        .map(|m| m.content.as_ref().map(|c| c.as_text()).unwrap_or_default())
        .unwrap_or_default();

    if query.trim().len() < 5 {
        return Ok(None);
    }

    let results = store
        .search_messages_in_conversation(&query, conversation_id, limit)
        .await?;

    if results.is_empty() {
        return Ok(None);
    }

    let mut block = String::from("<retrieved_context>\nRelevant context from conversation history:\n\n");
    for msg in &results {
        if let Some(content) = &msg.content {
            let text = content.as_text();
            let snippet = truncate_chars(&text, 300);
            block.push_str(&format!("[{}] {}\n", msg.role, snippet));
        }
    }
    block.push_str("</retrieved_context>");
    Ok(Some(block))
}
```

**Modified `summarize_and_replace`** (agent.rs:1395):

```rust
async fn summarize_and_replace(
    llm: &LlmClient,
    memory: &MemoryStore,
    conversation_id: &str,
    to_summarize: &[ChatMessage],
    preserved: &[ChatMessage],
    error_label: &str,
    summary_label: &str,
) -> Result<Vec<ChatMessage>> {

    // NEW: RAG retrieval for compaction (non-fatal — warn on error, continue)
    let retrieved = match memory::rag::retrieve_context_for_compaction(
        memory, to_summarize, preserved, conversation_id, 5
    ).await {
        Ok(r) => r,
        Err(e) => {
            warn!("RAG retrieval for compaction failed: {}", e);
            None
        }
    };

    // Build compact messages: summary prompt + retrieved context + truncated input
    let mut compact_msgs = Vec::new();
    compact_msgs.push(build_compact_summary_prompt());
    
    if let Some(ref ctx) = retrieved {
        compact_msgs.push(ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(ctx.clone())),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    // Tool-group-aware truncation: keep the first N tool groups (conversation
    // origin) and the last M tool groups (recent flow). This preserves the
    // structural integrity of tool-call→result pairs — unlike raw-index
    // truncation which could split a tool group and leave a dangling call
    // with no result.
    let groups = find_tool_groups(to_summarize);
    let bookend_groups = 1usize;
    let tail_groups = 3usize.min(groups.len().saturating_sub(bookend_groups));
    
    let mut sampled: Vec<ChatMessage> = Vec::new();
    let mut seen_indices = std::collections::HashSet::new();
    
    // First bookend groups (conversation origin)
    for group in groups.iter().take(bookend_groups) {
        seen_indices.insert(group.assistant_idx);
        for &ti in &group.tool_result_indices {
            seen_indices.insert(ti);
        }
    }
    
    // Last tail groups (recent flow)
    for group in groups.iter().rev().take(tail_groups) {
        seen_indices.insert(group.assistant_idx);
        for &ti in &group.tool_result_indices {
            seen_indices.insert(ti);
        }
    }
    
    // Always include any non-assistant/non-tool messages at the beginning
    // (conversation opening, user's first message, etc.)
    for (idx, msg) in to_summarize.iter().enumerate() {
        if msg.role != "assistant" && !msg.has_tool_calls() {
            seen_indices.insert(idx);
        }
    }
    
    // Build sampled list in original order, inserting truncation notice
    let mut inserted_notice = false;
    for (idx, msg) in to_summarize.iter().enumerate() {
        if seen_indices.contains(&idx) {
            sampled.push(msg.clone());
        } else if !inserted_notice {
            sampled.push(ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text(
                    format!("[... {} messages omitted, see retrieved_context above ...]",
                        to_summarize.len() - seen_indices.len())
                )),
                tool_calls: None,
                tool_call_id: None,
            });
            inserted_notice = true;
        }
    }

    let user_prompt = format!("Summarize the following conversation (sampled from {} messages):", to_summarize.len());
    compact_msgs.push(ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::Text(user_prompt)),
        tool_calls: None,
        tool_call_id: None,
    });
    compact_msgs.extend(sampled);

    // Existing LLM call
    let summary_response = match llm.chat(&compact_msgs, &[]).await { ... };
    // ... rest of existing logic ...
}
```

**New parameter** to `summarize_and_replace`: `memory: &MemoryStore, conversation_id: &str`.

**Updated callers:**
- `auto_compact_conversation` — passes `memory + conversation_id`
- `reactive_compact` — passes `memory + conversation_id`

**Existing callers of `summarize_and_replace`** (none other than the two above).

**Token impact:**

| Scenario | Before | After | Savings |
|----------|--------|-------|---------|
| 50 msgs, ~15K tokens | 15K sent to LLM | ~3K (retrieved + bookends) | 80% |
| 100 msgs, ~30K tokens | 30K sent | ~5K | 83% |
| 5 msgs, ~2K tokens | N/A — won't trigger | N/A | N/A |

**Edge cases:**
- `to_summarize` has no user messages → `retrieve_context_for_compaction` returns `None`, compact uses full `to_summarize` (fallback)
- Embeddings disabled (FTS5-only) → search falls back to FTS5-only, still works
- Search returns duplicates of what's already in bookends → RRF naturally ranks them, deduplication by `rowid`
- `to_summarize` and `preserved` both lack a user message → `retrieve_context_for_compaction` returns `None`, fallback to no RAG context
- Very small `to_summarize` (< 3 tool groups) → no truncation needed, send all

---

## Section 5: sqlite-vec + FTS5 Optimizations

### Problem

The current hybrid search (conversations.rs:189-267) works correctly but the RRF parameters are hardcoded and the vec0 table doesn't use metadata columns for pre-filtering.

### Solution

**Configurable RRF parameters** in `config.rs`:

```rust
pub struct MemoryConfig {
    // ... existing fields ...
    pub rrf_k: f64,            // default: 60.0
    pub rrf_weight_fts: f64,   // default: 0.5
    pub rrf_weight_vec: f64,   // default: 0.5
}
```

Update `search_messages_in_conversation` and `search_messages` to accept these parameters instead of hardcoded values.

**Optional vec0 metadata columns** (migration in `mod.rs:332-337`):

```sql
-- Current:
CREATE VIRTUAL TABLE message_embeddings USING vec0(embedding float[N]);

-- New (metadata columns for pre-filtering):
CREATE VIRTUAL TABLE message_embeddings USING vec0(
    embedding float[N],
    is_summarized integer,
    role text
);
```

Metadata columns allow KNN to filter at the vector level:
```sql
WHERE embedding MATCH ?1
  AND k = ?2
  AND is_summarized = 0
  AND role IN ('user', 'assistant')
```

This avoids post-filtering where valid KNN results get discarded by the WHERE clause after distance calculation. Migration re-creates the table on dimension change (already handled).

**Explicit migration** in `run_migrations` (mod.rs:350-374): Since `ALTER TABLE` is not supported for vec0 virtual tables, add a schema check on every startup:

```rust
// After the existing dimension-check migration block, add:
let has_metadata_columns = conn
    .prepare("PRAGMA table_info(message_embeddings)")
    .and_then(|mut stmt| {
        let cols: Vec<String> = stmt.query_map([], |row| row.get(1))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cols.contains(&"is_summarized".to_string()))
    })
    .unwrap_or(false);

if table_exists(conn, "message_embeddings") && !has_metadata_columns {
    conn.execute_batch("DROP TABLE message_embeddings;")?;
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE message_embeddings USING vec0(\
         embedding float[{}], is_summarized integer, role text);",
        dims
    ))?;
    info!("Migrated message_embeddings with metadata columns");
}
```

This is safe because `message_embeddings` is a derived index — dropping it loses no message data (messages are in the `messages` table). Embeddings will be regenerated on the next `save_message` call.

**Why this is safe:**
- `message_embeddings` is a derived index, not source-of-truth
- Messages are stored in the `messages` table (persistent)
- Vectors are regenerated on next `save_message` (mod.rs:67-75)
- The migration runs once; subsequent startups skip it
- Backward compatible with FTS5-only fallback

---

## Testing Strategy

**Context window detection:**
- Unit test: `fetch_context_window` returns `None` on API failure
- Unit test: `effective_context_window` falls back to static when cache is empty
- Unit test: `effective_context_window` returns cached value when set

**System prompt preservation:**
- Unit test: `auto_compact_conversation` preserves system messages in output
- Unit test: system messages are at indices 0..N in result (before any summary)
- Unit test: `reactive_compact` same behavior

**Structured summary:**
- Unit test: `build_compact_summary_prompt` produces prompt with STATE and CONTEXT keywords
- Unit test: `build_compact_boundary_marker` contains "STATE" hint

**RAG-aware compaction:**
- Integration test: `retrieve_context_for_compaction` returns snippets for real conversation
- Integration test: compact with retrieval returns valid state block
- Edge case test: empty retrieval falls back to full to_summarize

**sqlite-vec metadata:**
- Integration test: metadata columns are queryable in vec0
- Integration test: hybrid search with metadata pre-filter returns same results as post-filter
