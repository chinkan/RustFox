//! Convert a Markdown string to a `(plain_text, Vec<MessageEntity>)` pair suitable for
//! sending via the Telegram Bot API without any `parse_mode`.
//!
//! Inspired by [telegramify-markdown](https://github.com/sudoskys/telegramify-markdown)
//! by sudoskys.
//!
//! # Why entities instead of MarkdownV2?
//!
//! Telegram's `MarkdownV2` parse mode requires escaping 17+ special characters precisely.
//! Any mistake causes a 400 error and the bot falls back to raw unformatted text.  The
//! entity approach sends plain text alongside a list of formatting spans — no escaping
//! needed, zero risk of parse failures.
//!
//! # Telegram entity offset semantics
//!
//! Telegram measures entity offsets and lengths in **UTF-16 code units**, not bytes or
//! Unicode scalar values.  All offset conversions in this module use
//! `str[..n].encode_utf16().count()` to stay correct for CJK, emoji, and other
//! characters whose UTF-16 representation differs from UTF-8.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use teloxide::types::MessageEntity;
use tracing::warn;

/// Convert `markdown` to a `(plain_text, entities)` pair ready to pass to Telegram.
///
/// The returned `plain_text` contains no Markdown syntax — all formatting information
/// is encoded in the `entities` list.  Offsets and lengths in each entity are in
/// UTF-16 code units as required by the Telegram Bot API.
///
/// Supported conversions:
/// - `**bold**` → `Bold`
/// - `*italic*` / `_italic_` → `Italic`
/// - `` `code` `` → `Code`
/// - ` ```lang\n...\n``` ` → `Pre { language }`
/// - `[text](url)` → `TextLink { url }`
/// - `# Heading` / `## Heading` / `### Heading` → `Bold`
/// - `~~strikethrough~~` → `Strikethrough`
pub fn markdown_to_entities(markdown: &str) -> (String, Vec<MessageEntity>) {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_GFM);

    let parser = Parser::new_ext(markdown, options);

    let mut plain = String::new();
    let mut entities: Vec<MessageEntity> = Vec::new();

    // Stack of (tag, utf16_start_offset_in_plain_text)
    // We push on Start and pop+emit on End.
    let mut stack: Vec<(StackTag, usize)> = Vec::new();

    for event in parser {
        match event {
            // --- Text content ---
            Event::Text(text) => {
                plain.push_str(&text);
            }
            Event::Code(text) => {
                // Inline code: emit as a Code entity
                let start_utf16 = plain.encode_utf16().count();
                plain.push_str(&text);
                let end_utf16 = plain.encode_utf16().count();
                let length = end_utf16 - start_utf16;
                if length > 0 {
                    entities.push(MessageEntity::code(start_utf16, length));
                }
            }
            Event::SoftBreak => {
                plain.push('\n');
            }
            Event::HardBreak => {
                plain.push('\n');
            }

            // --- Block / inline formatting starts ---
            Event::Start(tag) => match tag {
                Tag::Strong => {
                    let utf16_off = plain.encode_utf16().count();
                    stack.push((StackTag::Bold, utf16_off));
                }
                Tag::Emphasis => {
                    let utf16_off = plain.encode_utf16().count();
                    stack.push((StackTag::Italic, utf16_off));
                }
                Tag::Strikethrough => {
                    let utf16_off = plain.encode_utf16().count();
                    stack.push((StackTag::Strikethrough, utf16_off));
                }
                Tag::Link { dest_url, .. } => {
                    let utf16_off = plain.encode_utf16().count();
                    stack.push((StackTag::Link(dest_url.to_string()), utf16_off));
                }
                Tag::Heading { .. } => {
                    let utf16_off = plain.encode_utf16().count();
                    stack.push((StackTag::Heading, utf16_off));
                }
                Tag::CodeBlock(kind) => {
                    let lang = match &kind {
                        CodeBlockKind::Fenced(lang) => {
                            let s = lang.trim().to_string();
                            if s.is_empty() {
                                None
                            } else {
                                Some(s)
                            }
                        }
                        CodeBlockKind::Indented => None,
                    };
                    let utf16_off = plain.encode_utf16().count();
                    stack.push((StackTag::CodeBlock(lang), utf16_off));
                }
                // Paragraph, list, etc. — no entity emitted on start.
                _ => {}
            },

            // --- Block / inline formatting ends ---
            Event::End(tag_end) => {
                match tag_end {
                    TagEnd::Strong => {
                        if let Some((StackTag::Bold, start)) = stack.pop() {
                            let end_utf16 = plain.encode_utf16().count();
                            let length = end_utf16.saturating_sub(start);
                            if length > 0 {
                                entities.push(MessageEntity::bold(start, length));
                            }
                        }
                    }
                    TagEnd::Emphasis => {
                        if let Some((StackTag::Italic, start)) = stack.pop() {
                            let end_utf16 = plain.encode_utf16().count();
                            let length = end_utf16.saturating_sub(start);
                            if length > 0 {
                                entities.push(MessageEntity::italic(start, length));
                            }
                        }
                    }
                    TagEnd::Strikethrough => {
                        if let Some((StackTag::Strikethrough, start)) = stack.pop() {
                            let end_utf16 = plain.encode_utf16().count();
                            let length = end_utf16.saturating_sub(start);
                            if length > 0 {
                                entities.push(MessageEntity::strikethrough(start, length));
                            }
                        }
                    }
                    TagEnd::Link => {
                        if let Some((StackTag::Link(url_str), start)) = stack.pop() {
                            let end_utf16 = plain.encode_utf16().count();
                            let length = end_utf16.saturating_sub(start);
                            if length > 0 {
                                // Parse the URL; if invalid, skip the entity (text is still kept)
                                if let Ok(url) = reqwest::Url::parse(&url_str) {
                                    entities.push(MessageEntity::text_link(url, start, length));
                                } else {
                                    warn!(
                                        "markdown_to_entities: invalid link URL ignored: {}",
                                        url_str
                                    );
                                }
                            }
                        }
                    }
                    TagEnd::Heading(_) => {
                        if let Some((StackTag::Heading, start)) = stack.pop() {
                            let end_utf16 = plain.encode_utf16().count();
                            let length = end_utf16.saturating_sub(start);
                            if length > 0 {
                                entities.push(MessageEntity::bold(start, length));
                            }
                        }
                        // Headings are block elements — add a newline after
                        plain.push('\n');
                    }
                    TagEnd::CodeBlock => {
                        if let Some((StackTag::CodeBlock(lang), start)) = stack.pop() {
                            // Trim trailing newline added by pulldown-cmark inside the block
                            if plain.ends_with('\n') {
                                plain.pop();
                            }
                            let end_utf16 = plain.encode_utf16().count();
                            let length = end_utf16.saturating_sub(start);
                            if length > 0 {
                                entities.push(MessageEntity::pre(lang, start, length));
                            }
                        }
                        plain.push('\n');
                    }
                    TagEnd::Paragraph => {
                        // Add blank line after each paragraph
                        plain.push('\n');
                    }
                    TagEnd::Item => {
                        plain.push('\n');
                    }
                    _ => {}
                }
            }

            // Ignore HTML, footnotes, rules, etc.
            _ => {}
        }
    }

    // Trim a single trailing newline that pulldown-cmark often adds
    if plain.ends_with('\n') {
        plain.pop();
    }

    (plain, entities)
}

/// Split a `(text, entities)` pair into chunks whose UTF-16 length does not exceed
/// `max_utf16_len`.  Offsets in child entity lists are adjusted to be relative to
/// each chunk's start.
///
/// Splitting tries to break at `\n` or space boundaries; it falls back to a hard
/// character boundary if no such split point exists in the window.
pub fn split_entities(
    text: &str,
    entities: &[MessageEntity],
    max_utf16_len: usize,
) -> Vec<(String, Vec<MessageEntity>)> {
    // Precompute cumulative UTF-16 lengths up to each char boundary to make
    // offset lookups O(1) instead of O(n).
    let char_utf16_boundaries: Vec<(usize, usize)> = {
        let mut v = Vec::new();
        let mut utf16_acc = 0usize;
        for (byte_pos, ch) in text.char_indices() {
            v.push((byte_pos, utf16_acc));
            utf16_acc += ch.len_utf16();
        }
        v.push((text.len(), utf16_acc));
        v
    };

    let total_utf16 = char_utf16_boundaries.last().map(|x| x.1).unwrap_or(0);

    if total_utf16 <= max_utf16_len {
        return vec![(text.to_string(), entities.to_vec())];
    }

    let mut result = Vec::new();
    let mut chunk_utf16_start = 0usize; // UTF-16 offset into original text where chunk starts

    while chunk_utf16_start < total_utf16 {
        let chunk_utf16_end_ideal = (chunk_utf16_start + max_utf16_len).min(total_utf16);

        // Find the byte position corresponding to chunk_utf16_end_ideal (or the last
        // char boundary at or before it).
        let split_utf16 = find_split_point(
            text,
            &char_utf16_boundaries,
            chunk_utf16_start,
            chunk_utf16_end_ideal,
        );

        // Convert UTF-16 offsets back to byte offsets for slicing
        let start_byte = utf16_to_byte(&char_utf16_boundaries, chunk_utf16_start);
        let end_byte = utf16_to_byte(&char_utf16_boundaries, split_utf16);

        let chunk_text = text[start_byte..end_byte].to_string();

        // Collect entities that overlap this chunk and adjust their offsets
        let chunk_entities: Vec<MessageEntity> = entities
            .iter()
            .filter_map(|e| {
                let e_start = e.offset;
                let e_end = e_start + e.length;
                let chunk_end = split_utf16;

                // Entity must overlap the chunk
                if e_end <= chunk_utf16_start || e_start >= chunk_end {
                    return None;
                }

                let clamped_start = e_start.max(chunk_utf16_start);
                let clamped_end = e_end.min(chunk_end);
                let new_offset = clamped_start - chunk_utf16_start;
                let new_length = clamped_end - clamped_start;

                if new_length == 0 {
                    return None;
                }

                let mut cloned = e.clone();
                cloned.offset = new_offset;
                cloned.length = new_length;
                Some(cloned)
            })
            .collect();

        result.push((chunk_text, chunk_entities));
        chunk_utf16_start = split_utf16;
    }

    result
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Internal tag discriminant stored on the formatting stack.
enum StackTag {
    Bold,
    Italic,
    Strikethrough,
    Link(String),
    Heading,
    CodeBlock(Option<String>),
}

/// Given a `(byte_pos, cumulative_utf16)` table, convert a UTF-16 offset to a byte offset.
/// Returns the byte position of the first char whose UTF-16 start is >= `utf16_off`.
fn utf16_to_byte(boundaries: &[(usize, usize)], utf16_off: usize) -> usize {
    match boundaries.binary_search_by_key(&utf16_off, |&(_, u)| u) {
        Ok(idx) => boundaries[idx].0,
        Err(idx) => {
            // idx is the insertion point — the byte just before
            if idx == 0 {
                0
            } else {
                boundaries[idx - 1].0
            }
        }
    }
}

/// Find a good UTF-16 split point at or before `ideal_end` that lands on a `\n` or
/// space character if possible, otherwise falls back to the exact boundary.
fn find_split_point(
    text: &str,
    boundaries: &[(usize, usize)],
    start_utf16: usize,
    ideal_end_utf16: usize,
) -> usize {
    if ideal_end_utf16 >= boundaries.last().map(|x| x.1).unwrap_or(0) {
        return ideal_end_utf16;
    }

    let ideal_byte = utf16_to_byte(boundaries, ideal_end_utf16);
    let start_byte = utf16_to_byte(boundaries, start_utf16);
    let window = &text[start_byte..ideal_byte];

    // Prefer newline, then space
    let split_byte_in_window = window
        .rfind('\n')
        .or_else(|| window.rfind(' '))
        .map(|pos| pos + 1); // keep the delimiter in the previous chunk

    let split_byte = split_byte_in_window
        .map(|off| start_byte + off)
        .unwrap_or(ideal_byte);

    // Convert split_byte back to utf16 offset
    match boundaries.binary_search_by_key(&split_byte, |&(b, _)| b) {
        Ok(idx) => boundaries[idx].1,
        Err(idx) => {
            if idx == 0 {
                start_utf16
            } else {
                boundaries[idx - 1].1
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity_kind_name(e: &MessageEntity) -> &str {
        use teloxide::types::MessageEntityKind::*;
        match &e.kind {
            Bold => "bold",
            Italic => "italic",
            Code => "code",
            Pre { .. } => "pre",
            TextLink { .. } => "text_link",
            Strikethrough => "strikethrough",
            _ => "other",
        }
    }

    // --- Basic formatting ---

    #[test]
    fn test_bold_converts_to_entity() {
        let (text, entities) = markdown_to_entities("**bold text**");
        assert_eq!(text, "bold text");
        assert_eq!(entities.len(), 1);
        assert_eq!(entity_kind_name(&entities[0]), "bold");
        assert_eq!(entities[0].offset, 0);
        assert_eq!(entities[0].length, 9);
    }

    #[test]
    fn test_italic_asterisk_converts_to_entity() {
        let (text, entities) = markdown_to_entities("*italic*");
        assert_eq!(text, "italic");
        assert_eq!(entities.len(), 1);
        assert_eq!(entity_kind_name(&entities[0]), "italic");
    }

    #[test]
    fn test_italic_underscore_converts_to_entity() {
        let (text, entities) = markdown_to_entities("_italic_");
        assert_eq!(text, "italic");
        assert_eq!(entities.len(), 1);
        assert_eq!(entity_kind_name(&entities[0]), "italic");
    }

    #[test]
    fn test_inline_code_converts_to_entity() {
        let (text, entities) = markdown_to_entities("`code`");
        assert_eq!(text, "code");
        assert_eq!(entities.len(), 1);
        assert_eq!(entity_kind_name(&entities[0]), "code");
    }

    #[test]
    fn test_fenced_code_block_converts_to_pre_entity() {
        let input = "```rust\nfn main() {}\n```";
        let (text, entities) = markdown_to_entities(input);
        assert_eq!(text, "fn main() {}");
        assert_eq!(entities.len(), 1);
        assert_eq!(entity_kind_name(&entities[0]), "pre");
        if let teloxide::types::MessageEntityKind::Pre {
            language: Some(lang),
        } = &entities[0].kind
        {
            assert_eq!(lang, "rust");
        } else {
            panic!("Expected Pre with language: {:?}", entities[0].kind);
        }
    }

    #[test]
    fn test_link_converts_to_text_link_entity() {
        let (text, entities) = markdown_to_entities("[RustFox](https://github.com)");
        assert_eq!(text, "RustFox");
        assert_eq!(entities.len(), 1);
        assert_eq!(entity_kind_name(&entities[0]), "text_link");
        if let teloxide::types::MessageEntityKind::TextLink { url } = &entities[0].kind {
            assert_eq!(url.as_str(), "https://github.com/");
        } else {
            panic!("Expected TextLink: {:?}", entities[0].kind);
        }
    }

    #[test]
    fn test_heading_converts_to_bold_entity() {
        let (text, entities) = markdown_to_entities("# Hello");
        assert!(
            text.contains("Hello"),
            "text must contain heading content: {text}"
        );
        assert!(
            entities.iter().any(|e| entity_kind_name(e) == "bold"),
            "heading must produce a bold entity"
        );
    }

    #[test]
    fn test_strikethrough_converts_to_entity() {
        let (text, entities) = markdown_to_entities("~~strikethrough~~");
        assert_eq!(text, "strikethrough");
        assert_eq!(entities.len(), 1);
        assert_eq!(entity_kind_name(&entities[0]), "strikethrough");
    }

    // --- UTF-16 offset correctness ---

    #[test]
    fn test_bold_with_cjk_correct_utf16_offsets() {
        // "你好 **world**" — "你好 " is 3 chars, each 1 UTF-16 unit
        let input = "你好 **world**";
        let (text, entities) = markdown_to_entities(input);
        assert!(text.contains("你好"), "CJK must appear in plain text");
        assert!(
            text.contains("world"),
            "bold text must appear in plain text"
        );

        let bold = entities
            .iter()
            .find(|e| entity_kind_name(e) == "bold")
            .unwrap();
        // UTF-16 offset of "world" after "你好 " (3 units)
        let expected_offset: usize = "你好 ".encode_utf16().count();
        assert_eq!(
            bold.offset, expected_offset,
            "UTF-16 offset must account for CJK chars"
        );
        assert_eq!(bold.length, 5); // "world" = 5 UTF-16 units
    }

    #[test]
    fn test_bold_with_emoji_correct_utf16_offsets() {
        // Emoji like 🦊 = 2 UTF-16 code units
        let input = "🦊 **bold**";
        let (text, entities) = markdown_to_entities(input);
        assert!(text.contains("bold"), "bold text must be in plain text");
        let bold = entities
            .iter()
            .find(|e| entity_kind_name(e) == "bold")
            .unwrap();
        // "🦊 " = 3 UTF-16 units (2 for emoji + 1 space)
        let expected_offset: usize = "🦊 ".encode_utf16().count();
        assert_eq!(
            bold.offset, expected_offset,
            "UTF-16 offset must account for surrogate-pair emoji"
        );
    }

    #[test]
    fn test_plain_text_no_entities() {
        let (text, entities) = markdown_to_entities("Hello world");
        assert_eq!(text, "Hello world");
        assert!(entities.is_empty(), "plain text must produce no entities");
    }

    #[test]
    fn test_empty_string_returns_empty() {
        let (text, entities) = markdown_to_entities("");
        assert!(text.is_empty());
        assert!(entities.is_empty());
    }

    #[test]
    fn test_mixed_bold_and_code() {
        let input = "**bold** and `code`";
        let (text, entities) = markdown_to_entities(input);
        assert!(text.contains("bold"), "bold text in output");
        assert!(text.contains("code"), "code text in output");
        assert!(entities.iter().any(|e| entity_kind_name(e) == "bold"));
        assert!(entities.iter().any(|e| entity_kind_name(e) == "code"));
    }

    // --- split_entities ---

    #[test]
    fn test_split_entities_short_text_not_split() {
        let (text, entities) = markdown_to_entities("**hello**");
        let chunks = split_entities(&text, &entities, 4096);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, "hello");
    }

    #[test]
    fn test_split_entities_long_text_splits() {
        let long = "a ".repeat(3000); // 6000 UTF-16 chars
        let (text, entities) = markdown_to_entities(&long);
        let chunks = split_entities(&text, &entities, 4096);
        assert!(chunks.len() > 1, "long text must be split");
        for (chunk_text, _) in &chunks {
            let utf16_len: usize = chunk_text.encode_utf16().count();
            assert!(
                utf16_len <= 4096,
                "chunk must not exceed max_utf16_len: {} > 4096",
                utf16_len
            );
        }
    }

    #[test]
    fn test_split_entities_entity_offsets_adjusted() {
        // Two bold words separated by enough filler to force a split
        let filler = " ".repeat(4090);
        let input = format!("**A**{}**B**", filler);
        let (text, entities) = markdown_to_entities(&input);
        let chunks = split_entities(&text, &entities, 4096);

        // The first chunk should have offset-0 entity for "A"
        let first_bold = chunks[0].1.iter().find(|e| entity_kind_name(e) == "bold");
        assert!(first_bold.is_some(), "first chunk must have a bold entity");
        assert_eq!(
            first_bold.unwrap().offset,
            0,
            "first chunk bold must start at offset 0"
        );
    }
}
