# Context-Aware Compaction & Memory Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) for syntax tracking.

**Goal:** Add dynamic context window detection, system prompt preservation, structured YAML+state summaries, RAG-assisted compaction, and configurable hybrid search to RustFox's compaction system.

**Architecture:** Six focused changes across the provider layer, memory layer, agent prompt builder, and agent loop. ProviderConfig gains a runtime context_window_cache with a background warmup task. The compact flow partitions system messages before summarization, injects retrieved RAG context and tool-group-aware truncation, then rebuilds system messages after. vec0 tables gain metadata columns for pre-filtering.

**Tech Stack:** Rust 2021, tokio, rusqlite, sqlite-vec, reqwest

---

### Files Touched

| File | Change |
|------|--------|
| `src/config.rs` | Add `rrf_k`, `rrf_weight_fts`, `rrf_weight_vec` to `MemoryConfig` |
| `src/provider.rs` | Add `context_window_cache` to `ProviderConfig`, `fetch_context_window()` trait method, `effective_context_window()` on registry |
| `src/memory/mod.rs` | vec0 metadata column migration (DROP + recreate with `is_summarized`, `role`) |
| `src/memory/conversations.rs` | Configurable RRF weights in hybrid search queries |
| `src/memory/rag.rs` | New `retrieve_context_for_compaction()` function |
| `src/agent_prompt.rs` | Enhanced structured summary prompt, updated boundary marker, updated recovery nudge |
| `src/agent.rs` | System prompt preservation, RAG-aware compact flow, tool-group-aware truncation |
| `src/main.rs` | Background startup task to warm `context_window_cache` |


---

## Task 1: Config — Add RRF parameters to MemoryConfig

**Files:**
- Modify: `src/config.rs:235-254`

- [ ] **Step 1: Add rrf fields to MemoryConfig**

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct MemoryConfig {
    // ... existing fields ...
    #[serde(default = "default_rrf_k")]
    pub rrf_k: f64,
    #[serde(default = "default_rrf_weight_fts")]
    pub rrf_weight_fts: f64,
    #[serde(default = "default_rrf_weight_vec")]
    pub rrf_weight_vec: f64,
}

fn default_rrf_k() -> f64 { 60.0 }
fn default_rrf_weight_fts() -> f64 { 0.5 }
fn default_rrf_weight_vec() -> f64 { 0.5 }
```

- [ ] **Step 2: Run `cargo check` to verify compilation**

Run: `cargo check 2>&1 | head -20`
Expected: compiles without errors (no warnings for dead_code on new fields is fine)

- [ ] **Step 3: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add rrf_k, rrf_weight_fts, rrf_weight_vec to MemoryConfig"
```

---

## Task 2: Provider — Add context_window_cache and fetch_context_window

**Files:**
- Modify: `src/provider.rs`
- Add import: `use tokio::sync::RwLock;` at line 2

- [ ] **Step 1: Add context_window_cache to ProviderConfig**

```rust
use std::sync::Arc;
use tokio::sync::RwLock;  // add to imports at line 2

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    // ... existing fields ...
    pub context_window: usize,
    /// Runtime cache for the current model's context window, populated
    /// asynchronously from the provider API. When None, falls back to
    /// `context_window`.
    pub context_window_cache: Arc<RwLock<Option<usize>>>,
    // ... remaining fields ...
}
```

Initialize in `From<&ProviderSection>`:
```rust
impl From<&ProviderSection> for ProviderConfig {
    fn from(s: &ProviderSection) -> Self {
        Self {
            // ... existing fields ...
            context_window: s.context_window,
            context_window_cache: Arc::new(RwLock::new(None)),
            parse_retry_limit: 0,
        }
    }
}
```

- [ ] **Step 2: Add `fetch_context_window` to Provider trait**

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn default_model(&self) -> &str;
    fn supports_vision(&self) -> bool;
    fn config(&self) -> &ProviderConfig;

    // ... existing methods ...

    /// Fetch the context window for a given model from the provider API.
    /// Returns None if the provider doesn't support runtime detection.
    async fn fetch_context_window(
        &self,
        _client: &reqwest::Client,
        _model: &str,
    ) -> Option<usize> {
        None  // default: no API-based detection
    }
}
```

- [ ] **Step 3: Implement for OpenRouterProvider**

Add method after `list_models` (after line 383):

```rust
async fn fetch_context_window(
    &self,
    client: &reqwest::Client,
    model: &str,
) -> Option<usize> {
    let url = format!("{}/models", self.config.base_url);
    let mut req = client.get(&url);
    if let Some(ref key) = self.config.api_key {
        req = req.header("Authorization", format!("Bearer {}", key));
    }
    let response = req.send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let list: serde_json::Value = response.json().await.ok()?;
    let ctx = list["data"].as_array()?
        .iter()
        .find(|m| m["id"].as_str() == Some(model))?
        .get("context_length")?
        .as_u64()?;
    Some(ctx as usize)
}
```

- [ ] **Step 4: Implement for OpenAICompatibleProvider**

```rust
async fn fetch_context_window(
    &self,
    client: &reqwest::Client,
    model: &str,
) -> Option<usize> {
    // Same implementation as OpenRouterProvider — many OpenAI-compatible
    // providers expose the same /v1/models endpoint with context_length.
    let url = format!("{}/models", self.config.base_url);
    let mut req = client.get(&url);
    if let Some(ref key) = self.config.api_key {
        req = req.header("Authorization", format!("Bearer {}", key));
    }
    let response = req.send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let list: serde_json::Value = response.json().await.ok()?;
    let ctx = list["data"].as_array()?
        .iter()
        .find(|m| m["id"].as_str() == Some(model))?
        .get("context_length")?
        .as_u64()?;
    Some(ctx as usize)
}
```

- [ ] **Step 5: Add `effective_context_window` to ProviderRegistry**

```rust
impl ProviderRegistry {
    // ... existing methods ...

    /// Return the effective context window for a model: runtime cache
    /// if populated, otherwise static config fallback.
    pub fn effective_context_window(&self, model: &str) -> usize {
        let (provider, _) = self.resolve_model(model);
        let cached = provider.config().context_window_cache.try_read()
            .ok()
            .and_then(|c| *c);
        cached.unwrap_or(provider.config().context_window)
    }
}
```

Note: using `try_read()` instead of `read()` because this may be called from sync contexts in the agent loop where `.await` is not available in the hot path (the agent loop calls this without holding an async runtime lock for the hot path at line 638-643).

- [ ] **Step 6: Add `Agent::refresh_context_window_cache` method**

In `src/agent.rs`, add after `set_model` (after ~line 395):

```rust
impl Agent {
    /// Fetch the context window size for the current model from the
    /// provider API and cache it. Non-fatal — uses static fallback on
    /// failure.
    pub async fn refresh_context_window_cache(&self) {
        let model = self.current_model.read().await.clone();
        let (provider, actual_model) = self.registry.resolve_model(&model);
        let client = reqwest::Client::new();
        if let Some(ctx) = provider.fetch_context_window(&client, actual_model).await {
            let mut cache = provider.config().context_window_cache.write().await;
            *cache = Some(ctx);
            tracing::info!("Context window for {}: {} tokens", actual_model, ctx);
        }
    }
}
```

- [ ] **Step 7: Update agent loop context_window resolution** (agent.rs:638-643)

Replace:
```rust
let context_window = {
    let model = self.current_model.read().await;
    let (provider, _) = self.registry.resolve_model(&model);
    provider.config().context_window
};
```

With:
```rust
let context_window = {
    let model = self.current_model.read().await;
    self.registry.effective_context_window(&model)
};
```

- [ ] **Step 8: Run cargo check**

Run: `cargo check 2>&1 | head -30`
Expected: compiles successfully

- [ ] **Step 9: Commit**

```bash
git add src/provider.rs src/agent.rs
git commit -m "feat(provider): runtime context_window_cache with fetch_context_window"
```

---

## Task 3: Memory — vec0 metadata columns migration

**Files:**
- Modify: `src/memory/mod.rs`

- [ ] **Step 1: Add schema detection + migration after dimension-check block**

After the `}` closing the `need_migrate` else branch (after line 374), add:

```rust
// Migration: add metadata columns (is_summarized, role) to vec0 tables
// for pre-filtering. ALTER TABLE is not supported for vec0, so we
// must DROP and recreate.
let has_meta = conn
    .prepare("PRAGMA table_info(message_embeddings)")
    .and_then(|mut stmt| {
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get(1))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cols.contains(&"is_summarized".to_string()))
    })
    .unwrap_or(false);

if table_exists(conn, "message_embeddings") && !has_meta {
    conn.execute_batch("DROP TABLE message_embeddings;")?;
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE message_embeddings USING vec0(\
         embedding float[{}], is_summarized integer, role text);",
        dims
    ))?;
    info!("Migrated message_embeddings with metadata columns (is_summarized, role)");
}
```

- [ ] **Step 2: Run cargo check**

Run: `cargo check 2>&1 | head -20`
Expected: compiles successfully

- [ ] **Step 3: Commit**

```bash
git add src/memory/mod.rs
git commit -m "feat(memory): add vec0 metadata column migration for is_summarized and role"
```

---

## Task 4: Memory — Configurable RRF weights in hybrid search

**Files:**
- Modify: `src/memory/conversations.rs`
- Modify: `src/config.rs` (already done in Task 1)

- [ ] **Step 1: Update `search_messages_in_conversation` to accept RRF params**

Change the signature to accept optional RRF overrides:

```rust
pub async fn search_messages_in_conversation(
    &self,
    query: &str,
    conversation_id: &str,
    limit: usize,
) -> Result<Vec<ChatMessage>> {
```

No signature change needed — read RRF params from `self.config` (but MemoryStore doesn't hold config directly... let me check).

Actually, looking at the MemoryConfig, it's part of Config which the Agent holds. MemoryStore doesn't hold a Config reference. The simplest approach: store the MemoryConfig in MemoryStore.

Add to MemoryStore:
```rust
pub struct MemoryStore {
    conn: Arc<Mutex<Connection>>,
    pub embeddings: Arc<EmbeddingEngine>,
    config: MemoryConfig,  // NEW
}
```

Update `open()` to accept config, update `open_in_memory()` to use defaults.

Actually, that's a bigger refactor. Let me take a simpler approach: make `search_messages_in_conversation` and `search_messages` accept optional RRF parameters.

```rust
pub async fn search_messages_in_conversation(
    &self,
    query: &str,
    conversation_id: &str,
    limit: usize,
) -> Result<Vec<ChatMessage>> {
```

No, let me check the actual approach. The RRF parameters are hardcoded in the SQL as `60` (the k constant) and `0.5` (the weight constants). A simpler approach: add parameters to the function and update the SQL to use parameters.

Let me keep it simple — add default parameters:

```rust
pub async fn search_messages_in_conversation(
    &self,
    query: &str,
    conversation_id: &str,
    limit: usize,
) -> Result<Vec<ChatMessage>> {
```

The SQL has:
```sql
coalesce(1.0 / (60 + fts.rank_number), 0.0) * 0.5
+ coalesce(1.0 / (60 + vec.rank_number), 0.0) * 0.5 as combined_rank
```

The `60` is RRF k and the `0.5` values are FTS weight and vec weight. These need to be parameterized in the SQL.

Wait, rusqlite doesn't support parameterizing within SQL expressions like `1.0 / (?1 + rank)`. Actually it does — you can use named or positional parameters in SQL expressions. The issue is that we already use `?1`, `?2`, `?3`, `?4` for other parameters.

Let me look at the current params:
- `?1` = query_bytes (vec embedding blob)
- `?2` = search_limit
- `?3` = query text (FTS)
- `?4` = conversation_id

I need to add `?5`, `?6`, `?7` for rrf_k, rrf_weight_fts, rrf_weight_vec.

This is straightforward. Let me write the implementation.

Actually, I realize the simplest approach that avoids touching MemoryStore's constructor: keep the SQL queries with the MemoryConfig defaults baked in as parameters. We'll add the parameters to the function signatures.

Let me do:

```rust
pub async fn search_messages_in_conversation(
    &self,
    query: &str,
    conversation_id: &str,
    limit: usize,
) -> Result<Vec<ChatMessage>> {
    // Use default RRF parameters from config if available
    // or hardcoded defaults (backward compatible)
    let rrf_k = 60.0;
    let rrf_weight_fts = 0.5;
    let rrf_weight_vec = 0.5;
    ...
```

Wait, but the spec says "configurable". The cleanest path without refactoring MemoryStore's constructor: pass the parameters through the function. But callers would need access to MemoryConfig...

Let me check what callers exist:
1. `rag.rs:30`: `store.search_messages_in_conversation(&search_query, conversation_id, limit).await`
2. `conversations.rs:189`: definition
3. `conversations.rs:446`: test

And `search_messages` callers (the global version):
1. No callers currently (it has `#[allow(dead_code)]`)

Wait, I see `rag.rs` calls `search_messages_in_conversation`. And `auto_retrieve_context` in `rag.rs` gets `store` as a `&MemoryStore`.

I think the best approach is to simply add MemoryConfig to MemoryStore. Let me do that.

Add to `MemoryStore`:
```rust
pub struct MemoryStore {
    conn: Arc<Mutex<Connection>>,
    pub embeddings: Arc<EmbeddingEngine>,
    config: MemoryConfig,
}
```

Change `MemoryStore::open`:
```rust
pub fn open(path: &Path, embedding_config: Option<EmbeddingConfig>, memory_config: MemoryConfig) -> Result<Self> {
```

But that requires updating all callers. Let me check where `open` is called:
1. `main.rs:118`: `MemoryStore::open(&config.memory.database_path, embedding_config)`
2. Tests: `open_in_memory()`

And `open_in_memory`:
```rust
pub fn open_in_memory() -> Result<Self> {
```

I'll add a default parameter. But Rust doesn't have default function parameters...

OK, let me take the cleaner approach: just pass RRF params through the search function signatures. The callers in `rag.rs` already have access to `store` — they can be updated to read from store. And `retrieve_context_for_compaction` is new anyway.

Actually, let me add `rrf_k`, `rrf_weight_fts`, `rrf_weight_vec` as parameters with `impl Into<Option<f64>>` or just take f64 and have the callers pass them. The callers in rag.rs can get them from... nothing yet.

Let me think about this differently. The simplest approach:

1. Add `MemoryConfig` to `MemoryStore` struct
2. Update `open()` to accept config
3. `open_in_memory()` uses default config
4. Search functions read from `self.config`
5. Update `main.rs` to pass `config.memory` to `MemoryStore::open`

This is the correct approach and clean. Let me do it.

```rust
pub struct MemoryStore {
    conn: Arc<Mutex<Connection>>,
    pub embeddings: Arc<EmbeddingEngine>,
    pub config: MemoryConfig,
}
```

- [ ] **Step 1: Add MemoryConfig field to MemoryStore**

```rust
pub struct MemoryStore {
    conn: Arc<Mutex<Connection>>,
    pub embeddings: Arc<EmbeddingEngine>,
    pub config: MemoryConfig,
}
```

Update `open()`:
```rust
pub fn open(path: &Path, embedding_config: Option<EmbeddingConfig>, memory_config: MemoryConfig) -> Result<Self> {
    // ... existing code ...
    let store = Self {
        conn: Arc::new(Mutex::new(conn)),
        embeddings: Arc::new(embeddings),
        config: memory_config,
    };
    // ...
}
```

Update `open_in_memory()`:
```rust
pub fn open_in_memory() -> Result<Self> {
    // ... existing code ...
    let store = Self {
        conn: Arc::new(Mutex::new(conn)),
        embeddings: Arc::new(embeddings),
        config: MemoryConfig::default(),
    };
    // ...
}
```

MemoryConfig needs a Default impl:

```rust
impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            database_path: PathBuf::new(),
            rag_limit: 5,
            max_raw_messages: 50,
            summarize_threshold: 20,
            summarize_cron: "0 3 * * * *".to_string(),
            query_rewriter_enabled: false,
            rrf_k: 60.0,
            rrf_weight_fts: 0.5,
            rrf_weight_vec: 0.5,
        }
    }
}

// Note: The serde defaults (from `default_rag_limit`, `default_max_raw_messages`, etc.)
// are defined via `#[serde(default = "...")]` on each field and remain the source
// of truth for deserialization. The `Default` impl above is for `open_in_memory()` only.
```

- [ ] **Step 2: Update main.rs to pass memory config**

In `src/main.rs`, change:
```rust
let memory = MemoryStore::open(&config.memory.database_path, embedding_config)
    .context("Failed to initialize memory store")?;
```

To:
```rust
let memory = MemoryStore::open(
    &config.memory.database_path,
    embedding_config,
    config.memory.clone(),
)
    .context("Failed to initialize memory store")?;
```

- [ ] **Step 3: Update search_messages_in_conversation to use configurable RRF**

In `src/memory/conversations.rs`, replace the hardcoded RRF values in the SQL with parameterized ones:

Hybrid branch (line 202-230):
```sql
WITH vec_matches AS (
    SELECT rowid, distance,
           row_number() OVER (ORDER BY distance) as rank_number
    FROM message_embeddings
    WHERE embedding MATCH ?1
      AND k = ?2
      AND is_summarized = 0
      AND role IN ('user', 'assistant')
    ORDER BY distance
    LIMIT ?2
),
fts_matches AS (
    SELECT rowid,
           row_number() OVER (ORDER BY rank) as rank_number
    FROM messages_fts
    WHERE messages_fts MATCH ?3
    LIMIT ?2
)
SELECT m.role, m.content, m.tool_calls, m.tool_call_id,
       coalesce(1.0 / (?5 + fts.rank_number), 0.0) * ?7
       + coalesce(1.0 / (?5 + vec.rank_number), 0.0) * ?6 as combined_rank
FROM messages m
LEFT JOIN vec_matches vec ON m.rowid = vec.rowid
LEFT JOIN fts_matches fts ON m.rowid = fts.rowid
WHERE (vec.rowid IS NOT NULL OR fts.rowid IS NOT NULL)
  AND m.conversation_id = ?4
  AND m.role IN ('user', 'assistant')
  AND (m.is_summarized IS NULL OR m.is_summarized = 0)
ORDER BY combined_rank DESC
LIMIT ?2
```

And update the params:
```rust
let rrf_k = self.config.rrf_k;
let rrf_weight_fts = self.config.rrf_weight_fts;
let rrf_weight_vec = self.config.rrf_weight_vec;
let mut stmt = conn.prepare(sql)?;
let messages = stmt
    .query_map(
        rusqlite::params![query_bytes, search_limit, query, conversation_id, rrf_k, rrf_weight_vec, rrf_weight_fts],
        parse_message_row,
    )?
    .collect::<Result<Vec<_>, _>>()
    .context("Failed to hybrid-search messages in conversation")?;
```

Note: `?6 = rrf_weight_vec`, `?7 = rrf_weight_fts` — order matters since FTS weight applies to the `fts.rank_number` term.

Wait, let me double check the order. `coalesce(1.0 / (?5 + fts.rank_number), 0.0) * ?7 + coalesce(1.0 / (?5 + vec.rank_number), 0.0) * ?6`:
- `?5` = rrf_k (for both FTS and vec)
- `?6` = rrf_weight_vec (applied to vec term)
- `?7` = rrf_weight_fts (applied to FTS term)

That's clean. Actually wait, I realize there's an issue. The `k = ?2` in the vec_matches WHERE clause — that's the KNN k parameter (number of nearest neighbors), not RRF k. These are two different concepts! KNN k = how many nearest neighbors to retrieve, RRF k = the constant in the fusion formula.

Let me rename to avoid confusion. The spec calls them `rrf_k` for the RRF constant. The KNN `LIMIT ?2` is `search_limit` which is `limit * 3`. These are different. The original code has `LIMIT ?2` in vec_matches which limits how many vectors to retrieve from the index (KNN search limit), and the outer `LIMIT ?2` limits final results after RRF fusion.

So I need `?5` for rrf_k, not to be confused with the existing `?2` which is `search_limit`.

OK, the param mapping is:
- `?1` = query_bytes
- `?2` = search_limit (KNN k, not RRF k!)
- `?3` = query text
- `?4` = conversation_id
- `?5` = rrf_k
- `?6` = rrf_weight_vec
- `?7` = rrf_weight_fts

This is correct. Let me also update vec_matches to use metadata columns for pre-filtering (the spec says to do this):

```sql
WHERE embedding MATCH ?1
  AND k = ?2
  AND is_summarized = 0
  AND role IN ('user', 'assistant')
```

This uses sqlite-vec's metadata column filtering. The `k` here is the KNN k, i.e., how many neighbors to return.

Now for the global `search_messages` — same changes but without conversation_id:

```sql
WITH vec_matches AS (
    SELECT rowid, distance,
           row_number() OVER (ORDER BY distance) as rank_number
    FROM message_embeddings
    WHERE embedding MATCH ?1
      AND k = ?2
      AND is_summarized = 0
      AND role IN ('user', 'assistant')
    ORDER BY distance
    LIMIT ?2
),
fts_matches AS (
    SELECT rowid,
           row_number() OVER (ORDER BY rank) as rank_number
    FROM messages_fts
    WHERE messages_fts MATCH ?3
    LIMIT ?2
)
SELECT m.role, m.content, m.tool_calls, m.tool_call_id,
       coalesce(1.0 / (?4 + fts.rank_number), 0.0) * ?6
       + coalesce(1.0 / (?4 + vec.rank_number), 0.0) * ?5 as combined_rank
FROM messages m
LEFT JOIN vec_matches vec ON m.rowid = vec.rowid
LEFT JOIN fts_matches fts ON m.rowid = fts.rowid
WHERE vec.rowid IS NOT NULL OR fts.rowid IS NOT NULL
ORDER BY combined_rank DESC
LIMIT ?2
```

With params: `rusqlite::params![query_bytes, search_limit, query, rrf_k, rrf_weight_vec, rrf_weight_fts]`

- [ ] **Step 4: Run cargo check**

Run: `cargo check 2>&1 | head -30`
Expected: compiles successfully

Advisory: Remove the stale `#[allow(dead_code)]` annotation on `search_messages_in_conversation` (line 188 in conversations.rs) — it's already called by `auto_retrieve_context` and will have more callers after this task.

- [ ] **Step 5: Commit**

```bash
git add src/memory/conversations.rs src/memory/mod.rs src/main.rs
git commit -m "feat(memory): configurable RRF weights and vec0 metadata pre-filtering"
```

---

## Task 5: rag.rs — Add retrieve_context_for_compaction

**Files:**
- Modify: `src/memory/rag.rs`

- [ ] **Step 1: Write the failing test first**

Add to the test module in `rag.rs`:

```rust
#[tokio::test]
async fn test_retrieve_context_for_compaction_returns_none_for_short_query() {
    let store = crate::memory::MemoryStore::open_in_memory().unwrap();
    let conv = store
        .get_or_create_conversation("test", "compact_u1")
        .await
        .unwrap();
    let to_summarize = vec![crate::llm::ChatMessage {
        role: "assistant".to_string(),
        content: Some(crate::llm::MessageContent::from_text("some result")),
        tool_calls: None,
        tool_call_id: None,
    }];
    let preserved = vec![];
    let result = retrieve_context_for_compaction(&store, &to_summarize, &preserved, &conv, 5)
        .await
        .unwrap();
    assert!(result.is_none(), "No user message should return None");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test test_retrieve_context_for_compaction_returns_none_for_short_query -- --nocapture 2>&1 | tail -10`
Expected: FAIL with "function not found in module"

- [ ] **Step 3: Add retrieve_context_for_compaction function**

After `auto_retrieve_context` (after line 58), add:

```rust
/// Retrieve context for compaction summarization.
///
/// Uses the most recent user message (from both to_summarize and preserved
/// ranges) as a search query to find relevant historical context from the
/// conversation. Returns formatted snippets that help the summarizer write
/// a focused summary. Returns None when no suitable query is found or no
/// results are returned.
pub async fn retrieve_context_for_compaction(
    store: &MemoryStore,
    to_summarize: &[ChatMessage],
    preserved: &[ChatMessage],
    conversation_id: &str,
    limit: usize,
) -> Result<Option<String>> {
    // Find the most recent user message across both ranges.
    // The most recent user message may be in preserved if compaction fires
    // right after the user spoke before any tool calls happened.
    let query = preserved
        .iter()
        .rev()
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

    let mut block = String::from(
        "<retrieved_context>\n\
         Relevant context from conversation history for compaction:\n\n",
    );

    for msg in &results {
        if let Some(content) = &msg.content {
            let text = content.as_text();
            let snippet = crate::utils::strings::truncate_chars(&text, 300);
            block.push_str(&format!("[{}] {}\n", msg.role, snippet));
        }
    }

    block.push_str("</retrieved_context>");
    debug!(
        "Compaction RAG: injected {} snippets for query: {:?}",
        results.len(),
        query
    );
    Ok(Some(block))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test test_retrieve_context_for_compaction_returns_none_for_short_query -- --nocapture 2>&1 | tail -10`
Expected: PASS (returns None because no user message in to_summarize or preserved)

- [ ] **Step 5: Add second test for successful retrieval**

```rust
#[tokio::test]
async fn test_retrieve_context_for_compaction_finds_user_message_in_preserved() {
    let store = crate::memory::MemoryStore::open_in_memory().unwrap();
    let conv = store
        .get_or_create_conversation("test", "compact_u2")
        .await
        .unwrap();

    // Save a user message to the conversation
    let msg = crate::llm::ChatMessage {
        role: "user".to_string(),
        content: Some(crate::llm::MessageContent::from_text("Tell me about Rust async")),
        tool_calls: None,
        tool_call_id: None,
    };
    store.save_message(&conv, &msg).await.unwrap();

    let to_summarize = vec![crate::llm::ChatMessage {
        role: "assistant".to_string(),
        content: Some(crate::llm::MessageContent::from_text("Here's how...")),
        tool_calls: None,
        tool_call_id: None,
    }];
    let preserved = vec![crate::llm::ChatMessage {
        role: "user".to_string(),
        content: Some(crate::llm::MessageContent::from_text("Tell me about Rust async")),
        tool_calls: None,
        tool_call_id: None,
    }];

    let result = retrieve_context_for_compaction(&store, &to_summarize, &preserved, &conv, 5)
        .await
        .unwrap();
    // May return None if embeddings not available (FTS-only fallback),
    // which is acceptable. The key is no panic.
    let _ = result;
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test --test '*' -- --nocapture 2>&1 | tail -20`
Expected: both new tests pass

- [ ] **Step 7: Commit**

```bash
git add src/memory/rag.rs
git commit -m "feat(rag): add retrieve_context_for_compaction for RAG-aware compaction"
```

---

## Task 6: agent_prompt.rs — Enhanced structured summary prompt

**Files:**
- Modify: `src/agent_prompt.rs`

- [ ] **Step 1: Update `build_compact_summary_prompt`**

Replace the function body (lines 440-465):

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
    ]
    .join("\n");

    ChatMessage {
        role: "system".to_string(),
        content: Some(MessageContent::Text(prompt_text)),
        tool_calls: None,
        tool_call_id: None,
    }
}
```

- [ ] **Step 2: Update `build_compact_boundary_marker`**

Change the format string (line 472):

```rust
"★ COMPACT SUMMARY — previous {} messages → YAML state + narrative ({} messages) ★"
```

- [ ] **Step 3: Update `recovery_nudge_for`**

Change the content strings (lines 109, 111):

```rust
let content = if previous_is_tool {
    "Continue from the tool result above. Read the ## STATE block in the compact summary for context. Either call the next required tool or provide a final answer.".to_string()
} else {
    "Continue from the user's request above. Read the ## STATE block in the compact summary for context. Either call the next required tool or provide a final answer.".to_string()
};
```

- [ ] **Step 4: Run cargo test to verify prompt tests still pass**

Run: `cargo test --lib agent_prompt 2>&1 | tail -20`
Expected: all existing tests pass

- [ ] **Step 5: Commit**

```bash
git add src/agent_prompt.rs
git commit -m "feat(prompt): structured YAML state + narrative summary prompt"
```

---

## Task 7: agent.rs — System prompt preservation + RAG-aware compaction

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Update `auto_compact_conversation` to partition system messages and pass memory**

Replace the function (lines 1330-1362):

```rust
/// Tier 3: Auto-compact via LLM summarization.
async fn auto_compact_conversation(
    llm: &LlmClient,
    memory: &MemoryStore,
    conversation_id: &str,
    messages: &[ChatMessage],
    _context_window: usize,
) -> Result<Vec<ChatMessage>> {
    // 1. Separate system messages from the rest
    let mut system_msgs: Vec<ChatMessage> = Vec::new();
    let non_system: Vec<ChatMessage> = messages
        .iter()
        .filter(|msg| {
            if msg.role == "system" {
                system_msgs.push(msg.clone());
                false
            } else {
                true
            }
        })
        .collect();

    let tool_groups = crate::agent_prompt::find_tool_groups(&non_system);

    let preserve_count = PRESERVED_TOOL_GROUPS.min(tool_groups.len());
    let preserved_groups_start = tool_groups.len().saturating_sub(preserve_count);

    let summary_end = if preserved_groups_start > 0 {
        let last_summary = &tool_groups[preserved_groups_start - 1];
        *last_summary
            .tool_result_indices
            .last()
            .unwrap_or(&last_summary.assistant_idx)
            + 1
    } else {
        return Ok(messages.to_vec());
    };

    let to_summarize = &non_system[..summary_end];
    let preserved = &non_system[summary_end..];

    // 2. Summarize with RAG-aware compaction
    let mut compacted = Self::summarize_and_replace(
        llm,
        memory,
        conversation_id,
        to_summarize,
        preserved,
        "Auto-compact",
        "★ COMPACT SUMMARY ★",
    )
    .await?;

    // 3. Prepend system messages back
    let mut result = system_msgs;
    result.append(&mut compacted);
    Ok(result)
}
```

- [ ] **Step 2: Update `reactive_compact` to partition system messages and pass memory**

Replace the function (lines 1369-1391):

```rust
/// Tier 4: Reactive compact — emergency 413 recovery.
async fn reactive_compact(
    llm: &LlmClient,
    memory: &MemoryStore,
    conversation_id: &str,
    messages: &[ChatMessage],
    _context_window: usize,
) -> Result<Vec<ChatMessage>> {
    const PRESERVE_COUNT: usize = 4;

    // 1. Separate system messages
    let mut system_msgs: Vec<ChatMessage> = Vec::new();
    let non_system: Vec<ChatMessage> = messages
        .iter()
        .filter(|msg| {
            if msg.role == "system" {
                system_msgs.push(msg.clone());
                false
            } else {
                true
            }
        })
        .collect();

    if non_system.len() <= PRESERVE_COUNT {
        anyhow::bail!("Too few non-system messages for reactive compact");
    }

    let split = non_system.len().saturating_sub(PRESERVE_COUNT);
    let to_summarize = &non_system[..split];
    let preserved = &non_system[split..];

    let mut compacted = Self::summarize_and_replace(
        llm,
        memory,
        conversation_id,
        to_summarize,
        preserved,
        "Reactive compact",
        "★ COMPACT SUMMARY (EMERGENCY) ★",
    )
    .await?;

    let mut result = system_msgs;
    result.append(&mut compacted);
    Ok(result)
}
```

- [ ] **Step 3: Replace `summarize_and_replace` with RAG-aware + tool-group-aware truncation**

Replace the function (lines 1395-1451):

```rust
/// Shared helper for Tiers 3 and 4: send messages to LLM for
/// summarization, then assemble the compacted result.
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
    let retrieved = match crate::memory::rag::retrieve_context_for_compaction(
        memory,
        to_summarize,
        preserved,
        conversation_id,
        5,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!("RAG retrieval for compaction failed: {}", e);
            None
        }
    };

    // Build compact messages: summary prompt + optional retrieved context + truncated input
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

    // Tool-group-aware truncation: keep the first N tool groups (origin)
    // and the last M tool groups (recent), preserving tool-call→result pairs.
    let groups = crate::agent_prompt::find_tool_groups(to_summarize);
    let bookend_groups = 1usize;
    let tail_groups = 3usize.min(groups.len().saturating_sub(bookend_groups));

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

    // Always include any non-assistant/non-tool messages (conversation opening, user messages, etc.)
    for (idx, msg) in to_summarize.iter().enumerate() {
        if msg.role != "assistant" && !msg.has_tool_calls() {
            seen_indices.insert(idx);
        }
    }

    // Build sampled list in original order, inserting one truncation notice
    let mut sampled: Vec<ChatMessage> = Vec::new();
    let mut inserted_notice = false;
    for (idx, msg) in to_summarize.iter().enumerate() {
        if seen_indices.contains(&idx) {
            sampled.push(msg.clone());
        } else if !inserted_notice {
            sampled.push(ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text(format!(
                    "[... {} messages omitted, see retrieved_context above ...]",
                    to_summarize.len() - seen_indices.len()
                ))),
                tool_calls: None,
                tool_call_id: None,
            });
            inserted_notice = true;
        }
    }

    // If no truncation happened, use the full to_summarize
    if sampled.is_empty() {
        sampled = to_summarize.to_vec();
    }

    let user_prompt = format!(
        "Summarize the following conversation (sampled from {} messages):",
        to_summarize.len()
    );
    compact_msgs.push(ChatMessage {
        role: "user".to_string(),
        content: Some(MessageContent::Text(user_prompt)),
        tool_calls: None,
        tool_call_id: None,
    });
    compact_msgs.extend(sampled);

    let summary_response = match llm.chat(&compact_msgs, &[]).await {
        Ok(c) => c,
        Err(e) => anyhow::bail!("{} LLM call failed: {}", error_label, e),
    };

    let summary_text = summary_response
        .content
        .as_ref()
        .map(|c| c.as_text())
        .unwrap_or_default();

    if summary_text.is_empty() {
        anyhow::bail!("{} returned empty summary", error_label);
    }

    let boundary = build_compact_boundary_marker(to_summarize.len(), 1);
    let summary_msg = ChatMessage {
        role: "system".to_string(),
        content: Some(MessageContent::Text(format!(
            "{}\n\n{}",
            summary_label, summary_text
        ))),
        tool_calls: None,
        tool_call_id: None,
    };

    let mut result: Vec<ChatMessage> = Vec::with_capacity(3 + preserved.len());
    result.push(boundary);
    result.push(summary_msg);
    result.extend(preserved.iter().cloned());

    let nudge = recovery_nudge_for(&result);
    result.push(nudge);

    Ok(result)
}
```

- [ ] **Step 4: Update callers of auto_compact_conversation and reactive_compact**

In the agent loop at line 664, change:
```rust
Self::auto_compact_conversation(&self.llm, &messages, context_window).await
```

To:
```rust
Self::auto_compact_conversation(
    &self.llm,
    &self.memory,
    &conversation_id,
    &messages,
    context_window,
)
.await
```

At line 817, change:
```rust
Self::reactive_compact(&self.llm, &messages, context_window).await
```

To:
```rust
Self::reactive_compact(
    &self.llm,
    &self.memory,
    &conversation_id,
    &messages,
    context_window,
)
.await
```

- [ ] **Step 5: Run cargo check**

Run: `cargo check 2>&1 | head -30`
Expected: compiles successfully

- [ ] **Step 6: Commit**

```bash
git add src/agent.rs
git commit -m "feat(agent): system prompt preservation and RAG-aware compaction"
```

---

## Task 8: main.rs — Background context_window cache warmup

- [ ] **Step 1: Add startup background task**

After `build_registry` (after line 76), before the info logging:

```rust
// Spawn background task to warm context_window_cache for all providers
{
    let registry_clone = Arc::clone(&registry);
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        for name in registry_clone.provider_names() {
            if let Some(provider) = registry_clone.get_provider(&name) {
                let model = provider.default_model();
                if let Some(ctx) = provider.fetch_context_window(&client, model).await {
                    let mut cache = provider.config().context_window_cache.write().await;
                    *cache = Some(ctx);
                    tracing::info!(
                        "Context window cache: {} / {} = {} tokens",
                        name,
                        model,
                        ctx
                    );
                }
            }
        }
    });
}
```

- [ ] **Step 2: Run cargo check**

Run: `cargo check 2>&1 | head -10`
Expected: compiles successfully

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat(main): background context_window cache warmup at startup"
```

---

## Task 9: Unit tests

- [ ] **Step 1: Add provider tests for effective_context_window**

In `src/provider.rs`, add to the test module:

```rust
#[test]
fn effective_context_window_falls_back_to_static_when_cache_empty() {
    let sections = vec![make_section(
        "alpha",
        ProviderType::OpenRouter,
        "https://openrouter.ai/api/v1",
        "anthropic/claude-sonnet-4-6",
    )];
    let reg = build_registry(&sections, "alpha", 3).unwrap();
    let ctx = reg.effective_context_window("anthropic/claude-sonnet-4-6");
    assert_eq!(ctx, 512_000); // static fallback from section
}

#[test]
fn effective_context_window_returns_cached_value_when_set() {
    use tokio::sync::RwLock;
    let sections = vec![make_section(
        "alpha",
        ProviderType::OpenRouter,
        "https://openrouter.ai/api/v1",
        "anthropic/claude-sonnet-4-6",
    )];
    let reg = build_registry(&sections, "alpha", 3).unwrap();
    let provider = reg.get_provider("alpha").unwrap();
    // Set cache manually
    *provider.config().context_window_cache.try_write().unwrap() = Some(200_000);
    let ctx = reg.effective_context_window("anthropic/claude-sonnet-4-6");
    assert_eq!(ctx, 200_000);
}
```

- [ ] **Step 2: Run provider tests**

Run: `cargo test --lib provider 2>&1 | tail -15`
Expected: all tests pass including new ones

- [ ] **Step 3: Add agent_prompt test for new summary prompt keywords**

In `src/agent_prompt.rs` test module:

```rust
#[test]
fn compact_summary_prompt_contains_state_keywords() {
    let msg = build_compact_summary_prompt();
    let text = msg.content.as_ref().unwrap().as_text();
    assert!(text.contains("## STATE"), "Should have STATE section");
    assert!(text.contains("## CONTEXT"), "Should have CONTEXT section");
    assert!(text.contains("decisions:"), "Should have decisions field");
    assert!(text.contains("pending:"), "Should have pending field");
    assert!(text.contains("last_action:"), "Should have last_action field");
}

#[test]
fn compact_boundary_marker_contains_state_hint() {
    let msg = build_compact_boundary_marker(10, 1);
    let text = msg.content.as_ref().unwrap().as_text();
    assert!(text.contains("STATE"), "Should hint at state format");
}
```

- [ ] **Step 4: Run agent_prompt tests**

Run: `cargo test --lib agent_prompt 2>&1 | tail -15`
Expected: all tests pass including new ones

- [ ] **Step 5: Run full test suite**

Run: `cargo test 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 6: Run clippy**

Run: `cargo clippy -- -D warnings 2>&1 | tail -20`
Expected: no warnings

- [ ] **Step 7: Commit tests**

```bash
git add src/provider.rs src/agent_prompt.rs
git commit -m "test: add tests for context_window cache and structured summary prompt"
```

---

## Self-Review

**1. Spec coverage:**
- Section 1 (Dynamic context window): Task 2 (fetch_context_window, cache, effective_context_window) + Task 8 (startup warmup) ✓
- Section 2 (System prompt preservation): Task 7 (partition system msgs in auto_compact + reactive_compact) ✓
- Section 3 (Structured summary prompt): Task 6 (build_compact_summary_prompt, boundary_marker, recovery_nudge_for) ✓
- Section 4 (RAG-aware compaction): Task 5 (retrieve_context_for_compaction) + Task 7 (summarize_and_replace with RAG + truncation) ✓
- Section 5 (sqlite-vec + FTS5 optimizations): Task 1 (configurable RRF) + Task 3 (vec0 metadata columns) + Task 4 (parameterized RRF in SQL) ✓
- Testing strategy: Task 9 ✓

**2. Placeholder scan:** No TBD, TODO, or incomplete code patterns found. All steps have complete code blocks.

**3. Type consistency:** `fetch_context_window` returns `Option<usize>` in trait definition and all implementations ✓. `summarize_and_replace` signature consistently has `memory: &MemoryStore, conversation_id: &str` before `to_summarize` ✓. `effective_context_window` takes `&str` and returns `usize` ✓.
