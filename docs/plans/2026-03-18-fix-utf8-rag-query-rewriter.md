# Fix UTF-8 Byte-Slice Panic in rag.rs and query_rewriter.rs

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Eliminate two `&s[..N]` byte-slice panics on multi-byte UTF-8 input (Chinese/Japanese) in `src/memory/rag.rs` and `src/memory/query_rewriter.rs` by extracting `truncate_chars` into a shared `src/utils/str.rs` utility and using it everywhere.

**Architecture:** A new `src/utils/str.rs` module exposes `pub fn truncate_chars` (already implemented privately in `tool_notifier.rs`). All three callers — `tool_notifier`, `rag`, and `query_rewriter` — use the shared version. This is a DRY refactor + bug fix in one pass. No new dependencies.

**Tech Stack:** Rust 2021, stdlib only.

---

## Background

The same byte-indexing panic fixed in `tool_notifier.rs` (`&s[..60]`) exists in two more files:

| File | Line | Broken code | Trigger |
|------|------|-------------|---------|
| `src/memory/rag.rs` | 45–46 | `&content[..300]` | any RAG result with Chinese text >300 bytes |
| `src/memory/query_rewriter.rs` | 97–98 | `&c[..200]` | any conversation message with Chinese text >200 bytes |

The existing test `test_format_history_truncates_long_content` in `query_rewriter.rs` only uses ASCII `"x".repeat(500)` — it will never catch this.

---

### Task 1: Create `src/utils/str.rs` Shared Utility

**Files:**
- Create: `src/utils/str.rs`
- Create: `src/utils/mod.rs`
- Modify: `src/main.rs` (add `mod utils;`)
- Modify: `src/platform/tool_notifier.rs` (replace private copy with shared one)

**Step 1: Write the test for the new module**

Create `src/utils/str.rs` with only the tests (no implementation yet):

```rust
/// Truncates `s` to at most `max_chars` Unicode scalar values.
/// Appends "..." if truncation occurred.
/// Safe for any UTF-8 input including Chinese, Japanese, emoji, etc.
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_chars_ascii_short() {
        assert_eq!(truncate_chars("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_chars_ascii_exact() {
        assert_eq!(truncate_chars("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_chars_ascii_truncated() {
        assert_eq!(truncate_chars("hello world", 5), "hello...");
    }

    #[test]
    fn test_truncate_chars_empty() {
        assert_eq!(truncate_chars("", 10), "");
    }

    #[test]
    fn test_truncate_chars_chinese_no_panic() {
        // Chinese chars are 3 bytes each — naive &s[..N] panics here
        let s = "每日上午10點 arXiv AI 論文摘要（香港時間）這是一段很長的中文文字用來測試截斷功能是否正確運作";
        let result = truncate_chars(s, 10);
        // Must not panic, must end with "..."
        assert!(result.ends_with("..."), "should truncate: {}", result);
        // Must be valid UTF-8
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
        // Must be at most 10 chars + "..." = 13 chars
        let char_count = result.chars().count();
        assert!(char_count <= 13, "too long: {} chars", char_count);
    }

    #[test]
    fn test_truncate_chars_chinese_short_no_ellipsis() {
        // String shorter than max_chars — no "..." appended
        let s = "你好世界";
        let result = truncate_chars(s, 10);
        assert_eq!(result, "你好世界");
    }

    #[test]
    fn test_truncate_chars_300_boundary() {
        // Simulate the rag.rs usage: 300-char limit
        let chinese = "香港時間每日簡報".repeat(50); // 8 chars * 50 = 400 chars
        let result = truncate_chars(&chinese, 300);
        assert!(result.ends_with("..."));
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }
}
```

**Step 2: Run tests to confirm they fail**

```bash
cargo test utils::str 2>&1
```

Expected: compile error or all tests fail with `not yet implemented` (from `todo!()`).

**Step 3: Implement `truncate_chars`**

Replace the `todo!()` in `src/utils/str.rs` with the real implementation:

```rust
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut byte_end = 0usize;
    for (char_count, ch) in s.chars().enumerate() {
        if char_count == max_chars {
            return format!("{}...", &s[..byte_end]);
        }
        byte_end += ch.len_utf8();
    }
    s.to_string()
}
```

**Step 4: Create `src/utils/mod.rs`**

```rust
pub mod str;
```

**Step 5: Declare `utils` in `src/main.rs`**

Add `mod utils;` to the module list at the top of `src/main.rs` (after the existing `mod tools;` line):

```rust
mod utils;
```

**Step 6: Update `src/platform/tool_notifier.rs` to use the shared version**

Remove the private `fn truncate_chars` from `tool_notifier.rs` (currently lines ~45–54) and replace all calls to `truncate_chars(...)` in that file with `crate::utils::str::truncate_chars(...)`.

The two call sites in `format_args_preview`:
```rust
// line ~32
let truncated = crate::utils::str::truncate_chars(&s, 60);

// line ~39
crate::utils::str::truncate_chars(args_json, 60)
```

**Step 7: Run all tests**

```bash
cargo test 2>&1
```

Expected: all tests pass. If `tool_notifier` tests fail, the refactor broke something — check the call sites.

**Step 8: Run lints**

```bash
cargo clippy -- -D warnings 2>&1 && cargo fmt --check 2>&1
```

Both must be clean. Run `cargo fmt` if format check fails.

**Step 9: Commit**

```bash
git add src/utils/str.rs src/utils/mod.rs src/main.rs src/platform/tool_notifier.rs
git commit -m "refactor(utils): extract truncate_chars to shared src/utils/str.rs

Move the UTF-8-safe string truncation helper from tool_notifier.rs
into a new src/utils/str module so rag.rs and query_rewriter.rs
can reuse it without duplicating the implementation."
```

---

### Task 2: Fix `src/memory/rag.rs`

**Files:**
- Modify: `src/memory/rag.rs` (lines 45–46)

**Step 1: Write a failing test**

Add this test at the bottom of `src/memory/rag.rs` (create a `#[cfg(test)] mod tests` block if one doesn't exist):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_retrieved_block_chinese_content_no_panic() {
        // Simulate what auto_retrieve_context does when content has Chinese chars
        // longer than 300 bytes. The old &content[..300] would panic here.
        // This calls the snippet-building logic indirectly via the format path.
        // We test truncate_chars directly since the inner loop isn't easily callable.
        let long_chinese = "每日論文摘要香港時間測試".repeat(30); // >300 bytes
        let result = crate::utils::str::truncate_chars(&long_chinese, 300);
        assert!(result.ends_with("..."));
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }
}
```

**Step 2: Run test to verify it compiles and passes** (it should already pass since Task 1 introduced `truncate_chars`)

```bash
cargo test memory::rag 2>&1
```

**Step 3: Fix the byte-slice**

In `src/memory/rag.rs` lines 45–49, replace:

```rust
let snippet = if content.len() > 300 {
    format!("{}...", &content[..300])
} else {
    content.clone()
};
```

With:

```rust
let snippet = crate::utils::str::truncate_chars(content, 300);
```

**Step 4: Run tests and lints**

```bash
cargo test 2>&1 && cargo clippy -- -D warnings 2>&1
```

All must pass.

**Step 5: Commit**

```bash
git add src/memory/rag.rs
git commit -m "fix(rag): replace byte-slice with truncate_chars to prevent UTF-8 panic

&content[..300] panics when byte 300 falls inside a multi-byte char.
Chinese/Japanese input in retrieved context triggered this path."
```

---

### Task 3: Fix `src/memory/query_rewriter.rs`

**Files:**
- Modify: `src/memory/query_rewriter.rs` (lines 97–98 and the existing test at lines 183–194)

**Step 1: Write a failing test for the Chinese input case**

The existing test `test_format_history_truncates_long_content` uses ASCII only and will not catch this bug. Add a new multibyte test alongside it (before the closing `}` of the `mod tests` block):

```rust
#[test]
fn test_format_history_truncates_long_chinese_no_panic() {
    // Old &c[..200] panics when byte 200 falls inside a multibyte char.
    // Chinese chars are 3 bytes each — 67 chars already exceed 200 bytes.
    let long_chinese = "每日論文摘要（香港時間）人工智能最新研究".repeat(15); // >200 bytes
    let msgs = vec![msg("user", &long_chinese)];
    let result = format_history(&msgs);
    // Must not panic
    assert!(!result.is_empty());
    assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    // Must be truncated
    assert!(result.contains("..."), "should truncate long content: {}", &result[..result.len().min(80)]);
}
```

**Step 2: Run to confirm test currently panics**

```bash
cargo test memory::query_rewriter::tests::test_format_history_truncates_long_chinese_no_panic 2>&1
```

Expected: FAILED with `byte index 200 is not a char boundary`.

**Step 3: Fix the byte-slice**

In `src/memory/query_rewriter.rs` lines 97–101, replace:

```rust
let snippet = if c.len() > 200 {
    format!("{}...", &c[..200])
} else {
    c.clone()
};
```

With:

```rust
let snippet = crate::utils::str::truncate_chars(c, 200);
```

**Step 4: Run all tests**

```bash
cargo test 2>&1 && cargo clippy -- -D warnings 2>&1 && cargo fmt --check 2>&1
```

All must pass. The new multibyte test and the old ASCII test should both be green.

**Step 5: Commit**

```bash
git add src/memory/query_rewriter.rs
git commit -m "fix(query_rewriter): replace byte-slice with truncate_chars to prevent UTF-8 panic

&c[..200] panics when byte 200 falls inside a multi-byte character.
Also adds a multibyte regression test — the existing test only used ASCII."
```

---

## Verification Checklist

- [ ] `src/utils/str.rs` exists with `pub fn truncate_chars` and 6+ tests
- [ ] `src/utils/mod.rs` exists with `pub mod str;`
- [ ] `mod utils;` declared in `src/main.rs`
- [ ] `tool_notifier.rs` no longer has a private `truncate_chars` — uses shared one
- [ ] `rag.rs:45–46` — no `&content[..300]`
- [ ] `query_rewriter.rs:97–98` — no `&c[..200]`
- [ ] `cargo test` — all pass including multibyte regression tests
- [ ] `cargo clippy -- -D warnings` — clean
- [ ] `cargo fmt --check` — clean
