# Fix UTF-8 Panic in tool_notifier.rs Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix a panic crash in `format_args_preview` caused by byte-indexed string slicing that breaks on multi-byte UTF-8 characters (Chinese/Japanese/etc).

**Architecture:** The fix replaces two unsafe `&s[..60]` byte-slices with a char-count-based truncation using `.chars().take(60).collect::<String>()`. This is pure Rust stdlib — no new dependencies needed. Both the single-arg fast-path (line 32-33) and the fallback raw-JSON path (line 43-44) in `format_args_preview` are affected.

**Tech Stack:** Rust 2021 edition, `serde_json`, no new dependencies.

---

## Background

**The crash:**
```
thread 'tokio-runtime-worker' panicked at src/platform/tool_notifier.rs:44:36:
byte index 60 is not a char boundary; it is inside '香' (bytes 58..61) of `{"description":"每日上午10點 arXiv AI 論文摘要（香港時間）",...`
```

**Why it crashes:**
- `String::len()` returns **byte length**, not character count
- `&s[..60]` requires byte index 60 to sit on a UTF-8 character boundary
- Chinese characters are 3 bytes each in UTF-8, so byte 60 can fall in the middle of one
- Rust panics rather than silently producing garbled output

**Two affected lines in `src/platform/tool_notifier.rs`:**

| Line | Code | Path |
|------|------|------|
| 32-33 | `if s.len() > 60 { format!("{}...", &s[..60]) }` | single-arg fast-path |
| 43-44 | `if args_json.len() > 60 { format!("{}...", &args_json[..60]) }` | fallback raw-JSON path |

**Safe replacement pattern:**
```rust
// BEFORE (panics on multibyte chars):
if s.len() > 60 {
    format!("{}...", &s[..60])
} else {
    s
}

// AFTER (safe):
let truncated: String = s.chars().take(60).collect();
if s.chars().count() > 60 {
    format!("{}...", truncated)
} else {
    s  // or truncated — same value
}
```

Or more efficiently (single-pass):
```rust
let mut char_count = 0usize;
let mut byte_end = 0usize;
for ch in s.chars() {
    if char_count == 60 { break; }
    char_count += 1;
    byte_end += ch.len_utf8();
}
if char_count == 60 {
    format!("{}...", &s[..byte_end])
} else {
    s
}
```

The simplest correct approach (collect into String) is fine for preview strings — no hot path concern.

---

### Task 1: Add Failing Tests for Multi-byte Characters

**Files:**
- Modify: `src/platform/tool_notifier.rs` (the `#[cfg(test)]` block, currently lines 172-232)

**Step 1: Add two failing tests in the existing test module**

Open `src/platform/tool_notifier.rs` and add these two tests inside the existing `#[cfg(test)] mod tests` block (after the last existing test, before the closing `}`):

```rust
#[test]
fn test_format_args_preview_single_arg_chinese() {
    // Chinese chars are 3 bytes each; byte index 60 falls mid-char without the fix.
    // "每日上午10點 arXiv AI 論文摘要（香港時間）" is >60 bytes but we want char-safe truncation.
    let long_chinese = "每日上午10點 arXiv AI 論文摘要（香港時間）很長的標題讓我們繼續寫下去直到超過六十個字";
    let json = format!(r#"{{"query":"{}"}}"#, long_chinese);
    // Must not panic — any result is acceptable as long as it doesn't crash.
    let preview = format_args_preview(&json);
    assert!(preview.contains("\""), "should be quoted single-arg preview");
    // Verify result is valid UTF-8 (it always is if we don't panic)
    assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
}

#[test]
fn test_format_args_preview_multi_arg_chinese_no_panic() {
    // Multi-arg JSON falls through to the raw-JSON fallback path (line 43-44).
    // This is the exact case that crashed in production.
    let args = r#"{"description":"每日上午10點 arXiv AI 論文摘要（香港時間）","prompt":"使用 arxiv-daily-briefing skill","trigger_type":"recurring","trigger_value":"0 0 2 * * *"}"#;
    // Must not panic
    let preview = format_args_preview(args);
    assert!(!preview.is_empty());
    assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
}
```

**Step 2: Run the tests to confirm they fail (or panic)**

```bash
cargo test -p rustfox test_format_args_preview_single_arg_chinese test_format_args_preview_multi_arg_chinese_no_panic 2>&1
```

Expected: one or both tests **panic** (not just fail) with `byte index N is not a char boundary`. If they don't panic yet, the strings may not be long enough — adjust the Chinese string to be longer.

> Note: A panic in a test shows as `FAILED` with the panic message in output.

**Step 3: Commit the failing tests**

```bash
git add src/platform/tool_notifier.rs
git commit -m "test(tool_notifier): add failing tests for multibyte UTF-8 truncation"
```

---

### Task 2: Fix the UTF-8 Slicing in `format_args_preview`

**Files:**
- Modify: `src/platform/tool_notifier.rs` lines 21-48

**Step 1: Read current `format_args_preview` to confirm line numbers**

The function currently looks like this (lines 21-48):

```rust
pub fn format_args_preview(args_json: &str) -> String {
    // Try to extract a single-value preview for readability
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_json) {
        if let Some(obj) = val.as_object() {
            if obj.len() == 1 {
                if let Some((_, v)) = obj.iter().next() {
                    let s = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let truncated = if s.len() > 60 {    // BUG: byte len
                        format!("{}...", &s[..60])       // BUG: byte slice
                    } else {
                        s
                    };
                    return format!("\"{}\"", truncated);
                }
            }
        }
    }
    // Fallback: truncate raw JSON
    if args_json.len() > 60 {                           // BUG: byte len
        format!("{}...", &args_json[..60])              // BUG: byte slice
    } else {
        args_json.to_string()
    }
}
```

**Step 2: Replace the function body with the safe version**

Replace the entire `format_args_preview` function with:

```rust
pub fn format_args_preview(args_json: &str) -> String {
    // Try to extract a single-value preview for readability
    // e.g. {"query":"Docker setup"} -> "Docker setup"
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(args_json) {
        if let Some(obj) = val.as_object() {
            if obj.len() == 1 {
                if let Some((_, v)) = obj.iter().next() {
                    let s = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let truncated = truncate_chars(&s, 60);
                    return format!("\"{}\"", truncated);
                }
            }
        }
    }
    // Fallback: truncate raw JSON
    truncate_chars(args_json, 60)
}

/// Truncate `s` to at most `max_chars` Unicode scalar values.
/// Appends "..." if truncation occurred.
/// Safe for any UTF-8 input including Chinese, Japanese, emoji, etc.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut char_count = 0usize;
    let mut byte_end = 0usize;
    for ch in s.chars() {
        if char_count == max_chars {
            // We have more chars — truncate here
            return format!("{}...", &s[..byte_end]);
        }
        char_count += 1;
        byte_end += ch.len_utf8();
    }
    // Fewer than max_chars — return as-is
    s.to_string()
}
```

**Step 3: Run all tool_notifier tests**

```bash
cargo test -p rustfox tool_notifier 2>&1
```

Expected: ALL tests pass including the two new ones. If any fail, read the error and fix before proceeding.

**Step 4: Run the full test suite and lints**

```bash
cargo test 2>&1 && cargo clippy -- -D warnings 2>&1 && cargo fmt --check 2>&1
```

Expected: all clean. If `cargo fmt --check` fails, run `cargo fmt` to auto-fix formatting.

**Step 5: Commit the fix**

```bash
git add src/platform/tool_notifier.rs
git commit -m "fix(tool_notifier): use char-safe truncation to prevent UTF-8 boundary panic

Byte-indexed slicing (&s[..60]) panics when the byte boundary falls
inside a multi-byte character (e.g. Chinese 香 = 3 bytes).
Replace with a single-pass char-counting loop that finds the safe
byte boundary, used in both the single-arg fast-path and the raw-JSON
fallback inside format_args_preview."
```

---

## Verification Checklist

- [ ] `cargo test` — all tests green including the two new multibyte tests
- [ ] `cargo clippy -- -D warnings` — no warnings
- [ ] `cargo fmt --check` — formatted
- [ ] The exact production input no longer panics:
  ```rust
  format_args_preview(r#"{"description":"每日上午10點 arXiv AI 論文摘要（香港時間）","prompt":"使用 arxiv-daily-briefing skill，生成每天的 AI 論文簡報（香港時間），並以廣東話摘要提供給用戶。","trigger_type":"recurring","trigger_value":"0 0 2 * * *"}"#);
  ```
