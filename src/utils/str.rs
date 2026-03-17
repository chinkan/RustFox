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
}
