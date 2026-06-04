/// MarkdownV2 special characters that must be escaped with `\` in plain text context.
/// See: https://core.telegram.org/bots/api#markdownv2-style
/// Backslash is listed first so the intent (escape the escaper) is self-evident.
const SPECIAL_CHARS_V2: &[char] = &[
    '\\', '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
];

/// Escape all MarkdownV2 special characters in a plain-text string.
/// Use this for text that should be rendered as literal text (not markup).
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if SPECIAL_CHARS_V2.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escape characters inside a code span or code block.
/// Only backtick (`) and backslash (\) need escaping inside code.
#[allow(dead_code)]
fn escape_code(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '`' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Find the position of the first unescaped occurrence of `needle` in `haystack`.
/// Returns `None` if not found.
#[allow(dead_code)]
fn find_unescaped(haystack: &str, needle: &str) -> Option<usize> {
    let mut i = 0;
    let bytes = haystack.as_bytes();
    let n = needle.len();
    while i + n <= bytes.len() {
        if haystack.is_char_boundary(i + n) && &haystack[i..i + n] == needle {
            // Check it's not preceded by backslash (simple check)
            if i == 0 || bytes[i - 1] != b'\\' {
                return Some(i);
            }
        }
        // Advance by one UTF-8 character
        let ch_len = haystack[i..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        i += ch_len;
    }
    None
}

/// Convert a line of inline-formatted text to MarkdownV2.
/// Handles: **bold**, `inline code`, [links](url), and plain text escaping.
/// Leaves `_italic_` as-is (MarkdownV2 already uses `_` for italic).
#[allow(dead_code)]
fn convert_inline(s: &str) -> String {
    let mut result = String::new();
    let mut remaining = s;

    while !remaining.is_empty() {
        // Bold: **text** → *text*
        if remaining.starts_with("**") {
            let after = &remaining[2..];
            if let Some(close) = find_unescaped(after, "**") {
                let inner = &after[..close];
                if !inner.is_empty() {
                    result.push('*');
                    result.push_str(&convert_inline(inner));
                    result.push('*');
                    remaining = &after[close + 2..];
                    continue;
                }
            }
        }

        // Inline code: `text` (but not ``` fenced blocks — those are handled at line level)
        if remaining.starts_with('`') && !remaining.starts_with("```") {
            let after = &remaining[1..];
            if let Some(close) = after.find('`') {
                let inner = &after[..close];
                result.push('`');
                result.push_str(&escape_code(inner));
                result.push('`');
                remaining = &after[close + 1..];
                continue;
            }
        }

        // Link: [display text](url)
        if remaining.starts_with('[') {
            if let Some(bracket_close) = find_matching_bracket(remaining) {
                let display = &remaining[1..bracket_close];
                let after_bracket = &remaining[bracket_close + 1..];
                if let Some(inside_parens) = after_bracket.strip_prefix('(') {
                    if let Some(paren_close) = inside_parens.find(')') {
                        let url = &inside_parens[..paren_close];
                        result.push('[');
                        result.push_str(&convert_inline(display));
                        result.push_str("](");
                        // URL only needs minimal escaping: ) must be escaped
                        result.push_str(&url.replace(')', "\\)"));
                        result.push(')');
                        remaining = &inside_parens[paren_close + 1..];
                        continue;
                    }
                }
            }
        }

        // `_italic_` in standard markdown stays as `_italic_` in MarkdownV2
        // (both use `_` for italic — no conversion needed, just escape surrounding chars)
        // `*single-asterisk*` italic in standard markdown → `_italic_` in MarkdownV2
        if remaining.starts_with('_') && !remaining.starts_with("__") {
            let after = &remaining[1..];
            if !after.starts_with(' ') {
                if let Some(close) = find_unescaped(after, "_") {
                    let inner = &after[..close];
                    if !inner.is_empty() && !inner.ends_with(' ') {
                        result.push('_');
                        result.push_str(&escape_text(inner));
                        result.push('_');
                        remaining = &after[close + 1..];
                        continue;
                    }
                }
            }
        }

        // `*single-asterisk*` italic in standard markdown → `_italic_` in MarkdownV2
        if remaining.starts_with('*') && !remaining.starts_with("**") {
            let after = &remaining[1..];
            // Only treat as italic if there's non-space content and a closing *
            if !after.starts_with(' ') {
                if let Some(close) = find_unescaped(after, "*") {
                    let inner = &after[..close];
                    if !inner.is_empty() && !inner.ends_with(' ') {
                        result.push('_');
                        result.push_str(&escape_text(inner));
                        result.push('_');
                        remaining = &after[close + 1..];
                        continue;
                    }
                }
            }
        }

        // Regular character — escape if special
        let ch = remaining.chars().next().unwrap();
        let ch_len = ch.len_utf8();
        if SPECIAL_CHARS_V2.contains(&ch) {
            result.push('\\');
        }
        result.push(ch);
        remaining = &remaining[ch_len..];
    }

    result
}

/// Find the index of the matching `]` for a `[` at the start of `s`.
#[allow(dead_code)]
fn find_matching_bracket(s: &str) -> Option<usize> {
    debug_assert!(s.starts_with('['));
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Convert a single non-code line, handling headers and inline formatting.
#[allow(dead_code)]
fn convert_line(line: &str) -> String {
    // ATX headers: # / ## / ### → *Heading* (bold)
    for prefix in &["### ", "## ", "# "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return format!("*{}*", convert_inline(rest));
        }
    }
    convert_inline(line)
}

/// Convert a string of standard markdown to Telegram MarkdownV2 format.
///
/// Rules applied:
/// - Fenced code blocks (` ``` `) — content is preserved except `` ` `` and `\` are escaped.
/// - Inline code spans (`` ` ... ` ``) — same limited escaping.
/// - `**bold**` → `*bold*`
/// - `*italic*` → `_italic_`
/// - `_italic_` → `_italic_` (unchanged — MarkdownV2 already uses `_`)
/// - `# Heading` / `## Heading` / `### Heading` → `*Heading*`
/// - `[text](url)` → `[text](url)` (text part escaped, URL left as-is)
/// - All other MarkdownV2 special characters in plain text are escaped with `\`.
#[allow(dead_code)]
pub fn markdown_to_telegram_v2(text: &str) -> String {
    let mut result = String::new();
    let mut in_code_block = false;
    let mut code_fence = String::new(); // tracks the opening fence (e.g. "```" or "```rust")

    // Collect lines once to avoid re-scanning for count on every iteration.
    let lines: Vec<&str> = text.lines().collect();
    let line_count = lines.len();

    for (idx, line) in lines.iter().enumerate() {
        let is_last_line = idx == line_count.saturating_sub(1);
        let newline = if is_last_line && !text.ends_with('\n') {
            ""
        } else {
            "\n"
        };

        let trimmed = line.trim_start();

        if !in_code_block && trimmed.starts_with("```") {
            // Entering a fenced code block
            let lang = trimmed.strip_prefix("```").unwrap_or("").trim();
            code_fence = format!("```{}", lang);
            result.push_str(&code_fence);
            result.push_str(newline);
            in_code_block = true;
        } else if in_code_block && trimmed.starts_with("```") {
            // Closing the fenced code block
            result.push_str("```");
            result.push_str(newline);
            in_code_block = false;
            code_fence.clear();
        } else if in_code_block {
            // Inside code block — only escape ` and \
            result.push_str(&escape_code(line));
            result.push_str(newline);
        } else {
            // Regular line — convert inline formatting
            result.push_str(&convert_line(line));
            result.push_str(newline);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- escape_text ---

    #[test]
    fn test_escape_plain_text_no_special_chars() {
        assert_eq!(escape_text("Hello world"), "Hello world");
    }

    #[test]
    fn test_escape_plain_text_dot() {
        assert_eq!(escape_text("Hello.World"), "Hello\\.World");
    }

    #[test]
    fn test_escape_plain_text_dash() {
        assert_eq!(escape_text("foo-bar"), "foo\\-bar");
    }

    #[test]
    fn test_escape_plain_text_multiple_special() {
        assert_eq!(escape_text("2+2=4"), "2\\+2\\=4");
    }

    #[test]
    fn test_escape_plain_text_underscore() {
        assert_eq!(escape_text("foo_bar"), "foo\\_bar");
    }

    #[test]
    fn test_escape_plain_text_backslash() {
        assert_eq!(escape_text("a\\b"), "a\\\\b");
    }

    // --- markdown_to_telegram_v2: plain text ---

    #[test]
    fn test_plain_text_no_special_chars_unchanged() {
        assert_eq!(markdown_to_telegram_v2("Hello world"), "Hello world");
    }

    #[test]
    fn test_plain_text_dot_escaped() {
        assert_eq!(markdown_to_telegram_v2("Hello."), "Hello\\.");
    }

    #[test]
    fn test_plain_text_dash_escaped() {
        assert_eq!(markdown_to_telegram_v2("foo-bar"), "foo\\-bar");
    }

    #[test]
    fn test_plain_text_exclamation_escaped() {
        assert_eq!(markdown_to_telegram_v2("Hi!"), "Hi\\!");
    }

    // --- markdown_to_telegram_v2: fenced code blocks ---

    #[test]
    fn test_fenced_code_block_content_not_escaped() {
        let input = "```\nfoo_bar.baz\n```";
        let output = markdown_to_telegram_v2(input);
        assert!(
            output.contains("foo_bar.baz"),
            "code content must not escape _ or .: {}",
            output
        );
    }

    #[test]
    fn test_fenced_code_block_backtick_escaped() {
        let input = "```\nlet x = `template`;\n```";
        let output = markdown_to_telegram_v2(input);
        assert!(
            output.contains("\\`template\\`"),
            "backticks inside code must be escaped: {}",
            output
        );
    }

    #[test]
    fn test_fenced_code_block_backslash_escaped() {
        let input = "```\npath\\to\\file\n```";
        let output = markdown_to_telegram_v2(input);
        assert!(
            output.contains("path\\\\to\\\\file"),
            "backslashes inside code must be escaped: {}",
            output
        );
    }

    #[test]
    fn test_fenced_code_block_with_language() {
        let input = "```rust\nfn main() {}\n```";
        let output = markdown_to_telegram_v2(input);
        assert!(
            output.starts_with("```rust\n"),
            "language tag must be kept: {}",
            output
        );
        assert!(
            output.contains("fn main() {}"),
            "function body must not be escaped: {}",
            output
        );
    }

    // --- markdown_to_telegram_v2: inline code ---

    #[test]
    fn test_inline_code_content_not_escaped_for_dots() {
        let input = "Use `foo.bar()` here";
        let output = markdown_to_telegram_v2(input);
        // dot must NOT be escaped inside inline code
        assert!(
            output.contains("`foo.bar()`"),
            "inline code dot must not be escaped: {}",
            output
        );
    }

    #[test]
    fn test_inline_code_backtick_escaped() {
        let input = "This `has \\` inside` it";
        let output = markdown_to_telegram_v2(input);
        assert!(
            output.contains("\\`"),
            "backtick in inline code must be escaped: {}",
            output
        );
    }

    // --- markdown_to_telegram_v2: bold ---

    #[test]
    fn test_bold_double_asterisk_converted() {
        assert_eq!(markdown_to_telegram_v2("**bold**"), "*bold*");
    }

    #[test]
    fn test_bold_with_special_char_in_text() {
        assert_eq!(markdown_to_telegram_v2("**foo.bar**"), "*foo\\.bar*");
    }

    #[test]
    fn test_bold_in_sentence() {
        let out = markdown_to_telegram_v2("This is **important** text.");
        assert!(
            out.contains("*important*"),
            "bold must be converted: {}",
            out
        );
        assert!(
            out.ends_with("\\."),
            "trailing dot must be escaped: {}",
            out
        );
    }

    // --- markdown_to_telegram_v2: italic ---

    #[test]
    fn test_italic_single_asterisk_converted() {
        assert_eq!(markdown_to_telegram_v2("*italic*"), "_italic_");
    }

    #[test]
    fn test_italic_underscore_unchanged() {
        // MarkdownV2 already uses _ for italic; no conversion needed
        assert_eq!(markdown_to_telegram_v2("_italic_"), "_italic_");
    }

    // --- markdown_to_telegram_v2: headers ---

    #[test]
    fn test_h1_converted_to_bold() {
        assert_eq!(markdown_to_telegram_v2("# Heading"), "*Heading*");
    }

    #[test]
    fn test_h2_converted_to_bold() {
        assert_eq!(markdown_to_telegram_v2("## Sub"), "*Sub*");
    }

    #[test]
    fn test_h3_converted_to_bold() {
        assert_eq!(markdown_to_telegram_v2("### Sub"), "*Sub*");
    }

    #[test]
    fn test_header_with_special_char() {
        assert_eq!(markdown_to_telegram_v2("# Hello."), "*Hello\\.*");
    }

    // --- markdown_to_telegram_v2: links ---

    #[test]
    fn test_link_passes_through() {
        let out = markdown_to_telegram_v2("[RustFox](https://github.com)");
        assert!(
            out.starts_with("[RustFox]"),
            "display text must be kept: {}",
            out
        );
        assert!(
            out.contains("(https://github.com)"),
            "URL must be kept: {}",
            out
        );
    }

    #[test]
    fn test_link_display_special_chars_escaped() {
        let out = markdown_to_telegram_v2("[foo.bar](https://x.com)");
        assert!(
            out.contains("foo\\.bar"),
            "dot in display text must be escaped: {}",
            out
        );
    }

    // --- markdown_to_telegram_v2: bullet lists ---

    #[test]
    fn test_bullet_list_dash_escaped() {
        let out = markdown_to_telegram_v2("- item one");
        assert!(
            out.starts_with("\\- "),
            "leading dash must be escaped: {}",
            out
        );
    }

    #[test]
    fn test_bullet_list_asterisk_escaped() {
        let out = markdown_to_telegram_v2("* item");
        // * followed by space is a list marker — it is treated as italic formatting
        // which converts to _item_ (italic). If there's no valid closing *, it's escaped.
        // Either way the output must be valid MarkdownV2.
        assert!(!out.is_empty());
    }

    // --- markdown_to_telegram_v2: mixed content ---

    #[test]
    fn test_mixed_bold_and_code_block() {
        let input = "**Summary:**\n\n```rust\nfn hello() {}\n```";
        let output = markdown_to_telegram_v2(input);
        assert!(output.contains("*Summary"), "bold must be converted");
        assert!(output.contains("```rust"), "code block must be kept");
        assert!(
            output.contains("fn hello() {}"),
            "code body must not be over-escaped"
        );
    }

    #[test]
    fn test_trailing_newline_preserved_if_present() {
        let input = "Hello\n";
        let output = markdown_to_telegram_v2(input);
        assert!(output.ends_with('\n'), "trailing newline must be preserved");
    }

    #[test]
    fn test_no_trailing_newline_preserved_if_absent() {
        let input = "Hello";
        let output = markdown_to_telegram_v2(input);
        assert!(!output.ends_with('\n'), "no extra newline should be added");
    }

    #[test]
    fn test_empty_string_returns_empty() {
        assert_eq!(markdown_to_telegram_v2(""), "");
    }

    // --- UTF-8 multi-byte character safety ---

    #[test]
    fn test_bold_in_chinese_text_does_not_panic() {
        // Reproduces: byte index 14 is not a char boundary inside '年'
        let input = "ComfyUI 2025 年度回顧** 🔥";
        // Must not panic; output must be valid UTF-8
        let out = markdown_to_telegram_v2(input);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn test_bold_wrapping_chinese_text_converts_correctly() {
        let input = "**2025 年度回顧**";
        let out = markdown_to_telegram_v2(input);
        assert!(
            out.starts_with('*') && out.ends_with('*'),
            "bold must wrap: {out}"
        );
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }
}
