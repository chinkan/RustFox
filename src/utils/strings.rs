/// Keep the last `max_chars` characters of `s`. If `s` exceeds `max_chars`,
/// prepend `"...(truncated)\n"` to the tail.
/// Safe for any UTF-8 input.
pub fn truncate_tail(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    let prefix = "...(truncated)\n";
    let tail: String = s
        .chars()
        .skip(char_count.saturating_sub(max_chars))
        .collect();
    format!("{}{}", prefix, tail)
}

/// Truncates `s` to at most `max_chars` Unicode scalar values.
/// Appends "..." if truncation occurred.
/// Safe for any UTF-8 input including Chinese, Japanese, emoji, etc.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_tail_short_text() {
        let input = "hello world";
        let result = truncate_tail(input, 100);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_truncate_tail_exact() {
        let input = "hello";
        let result = truncate_tail(input, 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_tail_truncated() {
        let input = "abcdefghijklmnopqrstuvwxyz";
        let result = truncate_tail(input, 10);
        let prefix = "...(truncated)\n";
        assert!(result.starts_with(prefix));
        assert!(result.ends_with("qrstuvwxyz"));
        assert_eq!(result.len(), prefix.len() + "qrstuvwxyz".len());
    }

    #[test]
    fn test_truncate_tail_chinese() {
        let input = "每日上午10點 arXiv AI 論文摘要（香港時間）這是一段很長的中文文字";
        let result = truncate_tail(input, 10);
        assert!(result.starts_with("...(truncated)\n"));
        let char_count = result.chars().count();
        // 10 tail chars + 16 prefix chars
        assert!(char_count <= 27, "too long: {} chars", char_count);
    }

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
        let s = "每日上午10點 arXiv AI 論文摘要（香港時間）這是一段很長的中文文字用來測試截斷功能是否正確運作";
        let result = truncate_chars(s, 10);
        assert!(result.ends_with("..."), "should truncate: {}", result);
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
        let char_count = result.chars().count();
        assert!(char_count <= 13, "too long: {} chars", char_count);
    }

    #[test]
    fn test_truncate_chars_chinese_short_no_ellipsis() {
        let s = "你好世界";
        let result = truncate_chars(s, 10);
        assert_eq!(result, "你好世界");
    }

    #[test]
    fn test_truncate_chars_300_boundary() {
        let chinese = "香港時間每日簡報".repeat(50);
        let result = truncate_chars(&chinese, 300);
        assert!(result.ends_with("..."));
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }

    #[test]
    fn test_truncate_chars_zero_max() {
        // max_chars=0: every non-empty string is truncated immediately to "..."
        assert_eq!(truncate_chars("hello", 0), "...");
        // Empty string is never truncated (loop body never entered)
        assert_eq!(truncate_chars("", 0), "");
    }
}
