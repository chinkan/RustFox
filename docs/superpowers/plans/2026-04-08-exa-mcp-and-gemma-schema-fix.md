# Exa MCP + Gemma Schema Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Exa neural search as a selectable MCP server in the setup wizard and fix tool-schema validation errors that occur when using Google AI Studio (Gemma/Gemini) models via OpenRouter.

**Architecture:** Exa uses a remote HTTP MCP endpoint (`https://mcp.exa.ai/mcp`) bridged to stdio via `mcp-remote` (no Rust changes needed). The schema fix adds a `sanitize_schema()` function in `src/llm.rs` that strips `required` entries referencing undefined properties before every API call — making all models work without per-model branching.

**Tech Stack:** Rust, `serde_json`, `rmcp`, `npx mcp-remote`, HTML/JS (setup wizard)

---

## File Map

| File | Action | What changes |
|------|--------|--------------|
| `setup/index.html` | Modify | Add Exa entry to `MCP_CATALOG` JS array |
| `config.example.toml` | Modify | Add commented Exa `[[mcp_servers]]` example block |
| `src/llm.rs` | Modify | Add `sanitize_schema()` helper; apply it to every tool before building `ChatRequest` |

---

### Task 1: Add Exa to the setup wizard MCP catalog

**Files:**
- Modify: `setup/index.html` — line ~505–509 (the `MCP_CATALOG` JS array near the `threads` entry)

- [ ] **Step 1: Write a failing test**

Open `setup/index.html` in a browser or use a Node.js snippet to verify the current `MCP_CATALOG` does **not** contain an entry with `name:'exa'`:

```bash
node -e "
const fs = require('fs');
const src = fs.readFileSync('setup/index.html','utf8');
const match = src.match(/name:'exa'/);
console.log(match ? 'FAIL: already present' : 'PASS: not yet present');
"
```

Expected output: `PASS: not yet present`

- [ ] **Step 2: Add the Exa entry to `MCP_CATALOG`**

In `setup/index.html`, locate the line containing `{ name:'threads', ...}` (around line 509).
Insert the following new entry **immediately before** the `threads` line so it sits in the `'Knowledge & Data'` group alongside `tavily` and `context7`:

```javascript
  { name:'exa',              category:'Knowledge & Data',   desc:'Neural web & document search (remote)',        runner:'npx', args:['-y','mcp-remote','https://mcp.exa.ai/mcp','--header','x-api-key:${EXA_API_KEY}'],  envVars:['EXA_API_KEY'],                              link:'https://exa.ai/mcp' },
```

> Note: the `${EXA_API_KEY}` placeholder inside `args` is intentional — the setup wizard's `generateConfig()` function does **not** perform shell expansion on args strings; it writes them verbatim into the TOML file and the user's shell expands them at runtime. Confirm this behaviour in the `generateConfig()` function (around line 658–674) before proceeding. If the wizard writes args verbatim, change the args to just `['-y','mcp-remote','https://mcp.exa.ai/mcp']` and add the API key via `[mcp_servers.env]` instead:

```javascript
  { name:'exa',              category:'Knowledge & Data',   desc:'Neural web & document search (remote)',        runner:'npx', args:['-y','mcp-remote','https://mcp.exa.ai/mcp'],  envVars:['EXA_API_KEY'],                              link:'https://exa.ai/mcp' },
```

The setup wizard will emit:

```toml
[[mcp_servers]]
name = "exa"
command = "npx"
args = ["-y", "mcp-remote", "https://mcp.exa.ai/mcp"]
[mcp_servers.env]
EXA_API_KEY = "<user-entered value>"
```

`mcp-remote` will authenticate using the `EXA_API_KEY` env var because the Exa remote MCP server reads `process.env.EXA_API_KEY` inside the bridge process.

- [ ] **Step 3: Verify the entry renders correctly**

```bash
node -e "
const fs = require('fs');
const src = fs.readFileSync('setup/index.html','utf8');
const match = src.match(/name:'exa'/);
const category = src.match(/name:'exa'[^}]+category:'([^']+)'/);
console.log(match ? 'PASS: exa found' : 'FAIL: exa missing');
console.log(category ? 'PASS: category=' + category[1] : 'FAIL: no category');
"
```

Expected:
```
PASS: exa found
PASS: category=Knowledge & Data
```

- [ ] **Step 4: Commit**

```bash
git add setup/index.html
git commit -m "feat: add Exa neural search to setup wizard MCP catalog"
```

---

### Task 2: Add Exa example block to `config.example.toml`

**Files:**
- Modify: `config.example.toml` — append after the `brave-search` example block (around line 138)

- [ ] **Step 1: Write a failing test**

```bash
grep -c "exa" config.example.toml && echo "FAIL: already present" || echo "PASS: not yet present"
```

Expected: `PASS: not yet present`

- [ ] **Step 2: Append the Exa example block**

Add after the closing line of the `brave-search` example block in `config.example.toml`:

```toml

# Example: Exa neural search MCP (remote server, requires mcp-remote via npx)
# Get your API key at https://exa.ai → Dashboard → API Keys
#
# [[mcp_servers]]
# name = "exa"
# command = "npx"
# args = ["-y", "mcp-remote", "https://mcp.exa.ai/mcp"]
# [mcp_servers.env]
# EXA_API_KEY = "your-exa-api-key"
```

- [ ] **Step 3: Verify**

```bash
grep -A 8 "# Example: Exa" config.example.toml
```

Expected output shows the full block including `EXA_API_KEY`.

- [ ] **Step 4: Commit**

```bash
git add config.example.toml
git commit -m "docs: add Exa remote MCP server example to config.example.toml"
```

---

### Task 3: Add `sanitize_schema()` to `src/llm.rs`

**Background:** Google AI Studio (and Gemini/Gemma models routed via OpenRouter) rejects tool schemas where the `required` array references property names that are not defined in `properties`. This happens with MCP tools that ship incomplete schemas. The fix is a recursive sanitizer applied universally before every API call.

**Files:**
- Modify: `src/llm.rs`

- [ ] **Step 1: Write failing unit tests first**

Add to the `#[cfg(test)] mod tests` block at the bottom of `src/llm.rs`:

```rust
#[test]
fn test_sanitize_schema_removes_undefined_required() {
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" }
        },
        "required": ["name", "ghost_field"]
    });
    sanitize_schema(&mut schema);
    let req = schema["required"].as_array().unwrap();
    assert_eq!(req.len(), 1);
    assert_eq!(req[0], "name");
}

#[test]
fn test_sanitize_schema_removes_empty_required() {
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {},
        "required": []
    });
    sanitize_schema(&mut schema);
    assert!(schema.get("required").is_none(), "empty required should be removed");
}

#[test]
fn test_sanitize_schema_leaves_valid_required_untouched() {
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {
            "a": { "type": "string" },
            "b": { "type": "integer" }
        },
        "required": ["a", "b"]
    });
    sanitize_schema(&mut schema);
    let req = schema["required"].as_array().unwrap();
    assert_eq!(req.len(), 2);
}

#[test]
fn test_sanitize_schema_no_properties_no_panic() {
    let mut schema = serde_json::json!({
        "type": "object",
        "required": ["something"]
    });
    // Without a `properties` key, required should be left as-is (no crash)
    sanitize_schema(&mut schema);
    // Still present because we can't verify against missing properties
    assert!(schema.get("required").is_some());
}

#[test]
fn test_sanitize_schema_recurses_into_nested_properties() {
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {
            "inner": {
                "type": "object",
                "properties": { "x": { "type": "string" } },
                "required": ["x", "nonexistent"]
            }
        },
        "required": []
    });
    sanitize_schema(&mut schema);
    let inner_req = schema["properties"]["inner"]["required"].as_array().unwrap();
    assert_eq!(inner_req.len(), 1);
    assert_eq!(inner_req[0], "x");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test test_sanitize_schema 2>&1 | tail -20
```

Expected: compile error — `sanitize_schema` is not defined yet.

- [ ] **Step 3: Implement `sanitize_schema()` in `src/llm.rs`**

Add this function **before** the `LlmClient` impl block (after the `parse_sse_content` function, around line 97):

```rust
/// Sanitize a JSON Schema so it is accepted by strict providers (e.g. Google AI Studio).
///
/// Rules applied:
/// 1. If both `properties` and `required` are present, filter `required` to only include
///    keys that actually exist in `properties`.
/// 2. After filtering, remove a `required` key entirely if the resulting array is empty.
/// 3. Recurse into nested `properties` values and `items` schemas.
fn sanitize_schema(schema: &mut serde_json::Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    // Collect defined property names (if any)
    let defined_props: Option<std::collections::HashSet<String>> = obj
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|p| p.keys().cloned().collect());

    if let Some(ref props) = defined_props {
        if let Some(required) = obj.get_mut("required") {
            if let Some(req_arr) = required.as_array_mut() {
                req_arr.retain(|v| v.as_str().map(|s| props.contains(s)).unwrap_or(false));
            }
        }
    }

    // Remove empty required array
    if obj
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(false)
    {
        obj.remove("required");
    }

    // Recurse into nested property schemas
    if let Some(properties) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
        for prop_schema in properties.values_mut() {
            sanitize_schema(prop_schema);
        }
    }

    // Recurse into array item schema
    if let Some(items) = obj.get_mut("items") {
        sanitize_schema(items);
    }
}
```

- [ ] **Step 4: Run the tests — verify they pass**

```bash
cargo test test_sanitize_schema 2>&1 | tail -20
```

Expected: all 5 tests pass.

- [ ] **Step 5: Apply `sanitize_schema()` in `chat_with_model()`**

In the `chat_with_model` method, after building `tools_param`, sanitize each tool's parameters schema before constructing the request.

Find the block (around line 119–136):

```rust
let tools_param = if tools.is_empty() {
    None
} else {
    Some(tools.to_vec())
};
```

Replace with:

```rust
let tools_param = if tools.is_empty() {
    None
} else {
    let mut sanitized = tools.to_vec();
    for tool in &mut sanitized {
        sanitize_schema(&mut tool.function.parameters);
    }
    Some(sanitized)
};
```

- [ ] **Step 6: Run all existing tests**

```bash
cargo test 2>&1 | tail -30
```

Expected: all tests pass (the schema serialization tests in `llm.rs` and config tests must still pass).

- [ ] **Step 7: Run clippy**

```bash
cargo clippy -- -D warnings 2>&1 | tail -20
```

Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add src/llm.rs
git commit -m "fix: sanitize tool parameter schemas for Google AI Studio / Gemma compatibility"
```

---

### Task 4: Final verification

- [ ] **Step 1: Full build check**

```bash
cargo build 2>&1 | tail -20
```

Expected: builds cleanly with no errors.

- [ ] **Step 2: Run full test suite**

```bash
cargo test 2>&1 | tail -30
```

Expected: all tests pass.

- [ ] **Step 3: Format check**

```bash
cargo fmt --all -- --check 2>&1
```

Expected: no diffs.

- [ ] **Step 4: Verify Exa entry appears in the setup HTML**

```bash
grep -A 2 "name:'exa'" setup/index.html
```

Expected: shows category `'Knowledge & Data'` and `envVars:['EXA_API_KEY']`.

- [ ] **Step 5: Verify Exa example in config**

```bash
grep -A 8 "Example: Exa" config.example.toml
```

Expected: shows the full commented example block.

---

## Self-Review Checklist

### Spec coverage
- [x] Add `https://mcp.exa.ai/mcp` to setup MCP list → Task 1 (setup wizard) + Task 2 (config.example.toml)
- [x] Fix Gemma schema `required` property-not-defined error → Task 3
- [x] All current models still work (sanitizer is a no-op for valid schemas) → Task 3 step 3 rule applied universally

### No placeholders
- All code blocks are complete and copy-pasteable
- All commands have expected output
- No "TBD" or "TODO" entries

### Type consistency
- `sanitize_schema` takes `&mut serde_json::Value` in both definition and call site
- `tool.function.parameters` is `serde_json::Value` in `FunctionDefinition` (confirmed in `src/llm.rs` line 44)
