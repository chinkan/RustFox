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
use teloxide::types::{MessageEntity, MessageEntityKind};
use tracing::warn;

/// Private Use Area sentinels for Telegram-specific inline formatting.
const SPOILER_START: char = '\u{E000}';
const SPOILER_END: char = '\u{E001}';
const UL_START: char = '\u{E002}';
const UL_END: char = '\u{E003}';

fn preprocess_markdown(md: &str) -> String {
    let ul_open: String = [UL_START, 'U'].iter().collect();
    let ul_close: String = [UL_END, '/', 'u'].iter().collect();
    let spoiler_open: String = [SPOILER_START, 'S'].iter().collect();
    let spoiler_close: String = [SPOILER_END, '/', 's'].iter().collect();

    let md = md.replace("<u>", &ul_open).replace("</u>", &ul_close);
    let re = regex::Regex::new(r"\|\|(.*?)\|\|").unwrap();
    re.replace_all(&md, format!("{spoiler_open}$1{spoiler_close}"))
        .to_string()
}

fn postprocess_entities(plain: &mut String, entities: &mut Vec<MessageEntity>) {
    let mut utf16_offset = 0usize;
    let mut out = String::new();
    let mut stack: Vec<(MessageEntityKind, usize)> = Vec::new();
    let chars: Vec<char> = plain.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            c if c == SPOILER_START && i + 1 < chars.len() && chars[i + 1] == 'S' => {
                stack.push((MessageEntityKind::Spoiler, utf16_offset));
                i += 2;
                continue;
            }
            c if c == SPOILER_END
                && i + 2 < chars.len()
                && chars[i + 1] == '/'
                && chars[i + 2] == 's' =>
            {
                if let Some(idx) = stack
                    .iter()
                    .rposition(|(k, _)| *k == MessageEntityKind::Spoiler)
                {
                    let (_, start) = stack.remove(idx);
                    let len = utf16_offset - start;
                    if len > 0 {
                        entities.push(MessageEntity::spoiler(start, len));
                    }
                }
                i += 3;
                continue;
            }
            c if c == UL_START && i + 1 < chars.len() && chars[i + 1] == 'U' => {
                stack.push((MessageEntityKind::Underline, utf16_offset));
                i += 2;
                continue;
            }
            c if c == UL_END
                && i + 2 < chars.len()
                && chars[i + 1] == '/'
                && chars[i + 2] == 'u' =>
            {
                if let Some(idx) = stack
                    .iter()
                    .rposition(|(k, _)| *k == MessageEntityKind::Underline)
                {
                    let (_, start) = stack.remove(idx);
                    let len = utf16_offset - start;
                    if len > 0 {
                        entities.push(MessageEntity::underline(start, len));
                    }
                }
                i += 3;
                continue;
            }
            _ => {
                out.push(chars[i]);
                utf16_offset += chars[i].len_utf16();
                i += 1;
            }
        }
    }
    *plain = out;
}

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

    let processed = preprocess_markdown(markdown);
    let parser = Parser::new_ext(&processed, options);

    let mut plain = String::new();
    let mut entities: Vec<MessageEntity> = Vec::new();

    // Stack of (tag, utf16_start_offset_in_plain_text)
    // We push on Start and pop+emit on End.
    let mut stack: Vec<(StackTag, usize)> = Vec::new();

    // Track UTF-16 length incrementally to avoid O(n²) rescanning
    let mut plain_utf16_len = 0usize;

    // State for blockquote entity
    let mut blockquote_start: Option<usize> = None;

    // State for list rendering: None = unordered, Some(n) = ordered starting at n
    let mut list_counter: Option<usize> = None;
    let mut needs_list_prefix = false;

    for event in parser {
        match event {
            // --- Text content ---
            Event::Text(text) => {
                if needs_list_prefix {
                    let prefix: String = match list_counter {
                        None => "\u{2022} ".to_string(),
                        Some(ref mut n) => {
                            let p = format!("{}. ", n);
                            *n += 1;
                            p
                        }
                    };
                    plain.push_str(&prefix);
                    plain_utf16_len += prefix.encode_utf16().count();
                    needs_list_prefix = false;
                }
                plain.push_str(&text);
                plain_utf16_len += text.encode_utf16().count();
            }
            Event::Code(text) => {
                // Inline code: emit as a Code entity
                let start_utf16 = plain_utf16_len;
                plain.push_str(&text);
                let text_utf16_len = text.encode_utf16().count();
                plain_utf16_len += text_utf16_len;
                let length = text_utf16_len;
                if length > 0 {
                    entities.push(MessageEntity::code(start_utf16, length));
                }
            }
            Event::SoftBreak => {
                plain.push('\n');
                plain_utf16_len += 1;
            }
            Event::HardBreak => {
                plain.push('\n');
                plain_utf16_len += 1;
            }

            // --- Block / inline formatting starts ---
            Event::Start(tag) => match tag {
                Tag::Strong => {
                    stack.push((StackTag::Bold, plain_utf16_len));
                }
                Tag::Emphasis => {
                    stack.push((StackTag::Italic, plain_utf16_len));
                }
                Tag::Strikethrough => {
                    stack.push((StackTag::Strikethrough, plain_utf16_len));
                }
                Tag::Link { dest_url, .. } => {
                    stack.push((StackTag::Link(dest_url.to_string()), plain_utf16_len));
                }
                Tag::Heading { .. } => {
                    stack.push((StackTag::Heading, plain_utf16_len));
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
                    stack.push((StackTag::CodeBlock(lang), plain_utf16_len));
                }
                Tag::BlockQuote(_) => {
                    blockquote_start = Some(plain_utf16_len);
                }
                Tag::Table(_) => {
                    // Table alignment metadata is discarded — rendered as plain text
                }
                Tag::TableHead | Tag::TableRow => {}
                Tag::TableCell => {}
                Tag::List(start) => {
                    list_counter = start.map(|n| n as usize);
                }
                Tag::Item => {
                    needs_list_prefix = true;
                }
                // Paragraph, list, etc. — no entity emitted on start.
                _ => {}
            },

            // --- Block / inline formatting ends ---
            Event::End(tag_end) => {
                match tag_end {
                    TagEnd::Strong => {
                        if let Some((StackTag::Bold, start)) = stack.pop() {
                            let length = plain_utf16_len.saturating_sub(start);
                            if length > 0 {
                                entities.push(MessageEntity::bold(start, length));
                            }
                        }
                    }
                    TagEnd::Emphasis => {
                        if let Some((StackTag::Italic, start)) = stack.pop() {
                            let length = plain_utf16_len.saturating_sub(start);
                            if length > 0 {
                                entities.push(MessageEntity::italic(start, length));
                            }
                        }
                    }
                    TagEnd::Strikethrough => {
                        if let Some((StackTag::Strikethrough, start)) = stack.pop() {
                            let length = plain_utf16_len.saturating_sub(start);
                            if length > 0 {
                                entities.push(MessageEntity::strikethrough(start, length));
                            }
                        }
                    }
                    TagEnd::Link => {
                        if let Some((StackTag::Link(url_str), start)) = stack.pop() {
                            let length = plain_utf16_len.saturating_sub(start);
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
                            let length = plain_utf16_len.saturating_sub(start);
                            if length > 0 {
                                entities.push(MessageEntity::bold(start, length));
                            }
                        }
                        // Headings are block elements — add a newline after
                        plain.push('\n');
                        plain_utf16_len += 1;
                    }
                    TagEnd::CodeBlock => {
                        if let Some((StackTag::CodeBlock(lang), start)) = stack.pop() {
                            // Trim trailing newline added by pulldown-cmark inside the block
                            if plain.ends_with('\n') {
                                plain.pop();
                                plain_utf16_len -= 1;
                            }
                            let length = plain_utf16_len.saturating_sub(start);
                            if length > 0 {
                                entities.push(MessageEntity::pre(lang, start, length));
                            }
                        }
                        plain.push('\n');
                        plain_utf16_len += 1;
                    }
                    TagEnd::Paragraph => {
                        // Add blank line after each paragraph (double newline to preserve paragraph breaks)
                        plain.push_str("\n\n");
                        plain_utf16_len += 2;
                    }
                    TagEnd::Item => {
                        plain.push('\n');
                        plain_utf16_len += 1;
                    }
                    TagEnd::BlockQuote(_) => {
                        if let Some(start) = blockquote_start.take() {
                            let length = plain_utf16_len.saturating_sub(start);
                            if length > 0 {
                                entities.push(MessageEntity {
                                    kind: MessageEntityKind::Blockquote,
                                    offset: start,
                                    length,
                                });
                            }
                        }
                        if !plain.ends_with('\n') {
                            plain.push('\n');
                            plain_utf16_len += 1;
                        }
                    }
                    TagEnd::TableCell => {
                        plain.push_str(" | ");
                        plain_utf16_len += 3;
                    }
                    TagEnd::TableHead | TagEnd::TableRow => {
                        // Remove trailing " | " and add newline
                        if plain.ends_with(" | ") {
                            plain.truncate(plain.len() - 3);
                            plain_utf16_len = plain_utf16_len.saturating_sub(3);
                        }
                        plain.push('\n');
                        plain_utf16_len += 1;
                    }
                    TagEnd::List(_) => {
                        list_counter = None;
                        needs_list_prefix = false;
                    }
                    _ => {}
                }
            }

            // Ignore HTML, footnotes, rules, etc.
            _ => {}
        }
    }

    // Trim trailing newlines (at most two, from the last paragraph's \n\n)
    while plain.ends_with('\n') && plain_utf16_len > 0 {
        plain.pop();
        plain_utf16_len -= 1;
    }

    postprocess_entities(&mut plain, &mut entities);

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
        let mut split_utf16 = find_split_point(
            text,
            &char_utf16_boundaries,
            chunk_utf16_start,
            chunk_utf16_end_ideal,
        );

        // Ensure progress even if `find_split_point` cannot find a boundary inside
        // the requested window (for example, when `max_utf16_len` is smaller than
        // a single character's UTF-16 length, such as an emoji/surrogate pair).
        if split_utf16 <= chunk_utf16_start {
            split_utf16 = char_utf16_boundaries
                .iter()
                .map(|&(_, utf16)| utf16)
                .find(|&utf16| utf16 > chunk_utf16_start)
                .unwrap_or(total_utf16);
        }

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
/// Returns the byte position of the char boundary at or just before `utf16_off`.
/// When `utf16_off` falls in the middle of a surrogate pair (i.e. it does not exactly
/// match any entry), the byte position of the preceding character is returned.
fn utf16_to_byte(boundaries: &[(usize, usize)], utf16_off: usize) -> usize {
    match boundaries.binary_search_by_key(&utf16_off, |&(_, u)| u) {
        Ok(idx) => boundaries[idx].0,
        Err(idx) => {
            // idx is the insertion point — snap to the preceding char boundary.
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

    // --- Blockquotes ---

    #[test]
    fn test_blockquote_emits_entity() {
        let (text, entities) = markdown_to_entities("> This is a quote");
        assert!(
            text.contains("This is a quote"),
            "blockquote text must be present: {text}"
        );
        assert!(
            !text.contains("> "),
            "blockquote must NOT have '> ' prefix when entity is used"
        );
        let blockquote = entities
            .iter()
            .find(|e| matches!(e.kind, MessageEntityKind::Blockquote));
        assert!(
            blockquote.is_some(),
            "blockquote must produce a Blockquote entity"
        );
    }

    // --- Tables ---

    #[test]
    fn test_table_renders_columns() {
        let input = "| A | B |\n|---|---|\n| 1 | 2 |";
        let (text, _) = markdown_to_entities(input);
        assert!(text.contains('A'), "column A must be in output: {text}");
        assert!(text.contains('B'), "column B must be in output: {text}");
        assert!(text.contains('1'), "row 1 col 1 must be in output: {text}");
        assert!(text.contains('2'), "row 1 col 2 must be in output: {text}");
    }

    // --- Spoilers and Underline ---

    #[test]
    fn test_spoiler_converts_to_entity() {
        let (text, entities) = markdown_to_entities("||hidden||");
        assert_eq!(text, "hidden");
        assert!(entities
            .iter()
            .any(|e| matches!(e.kind, MessageEntityKind::Spoiler)));
    }

    #[test]
    fn test_underline_converts_to_entity() {
        let (text, entities) = markdown_to_entities("<u>underlined</u>");
        assert_eq!(text, "underlined");
        assert!(entities
            .iter()
            .any(|e| matches!(e.kind, MessageEntityKind::Underline)));
    }

    #[test]
    fn test_spoiler_with_bold() {
        let (text, entities) = markdown_to_entities("**bold** and ||spoiler||");
        assert!(text.contains("bold"));
        assert!(text.contains("spoiler"));
        assert!(entities
            .iter()
            .any(|e| matches!(e.kind, MessageEntityKind::Bold)));
        assert!(entities
            .iter()
            .any(|e| matches!(e.kind, MessageEntityKind::Spoiler)));
    }

    // --- Lists ---

    #[test]
    fn test_unordered_list_renders_with_bullets() {
        let input = "- item one\n- item two";
        let (text, _) = markdown_to_entities(input);
        assert!(
            text.contains("\u{2022} item one"),
            "unordered list must use bullet: {text}"
        );
        assert!(
            text.contains("\u{2022} item two"),
            "second item must also have bullet: {text}"
        );
    }

    #[test]
    fn test_ordered_list_renders_with_numbers() {
        let input = "1. first\n2. second";
        let (text, _) = markdown_to_entities(input);
        assert!(
            text.contains("1. first"),
            "ordered list must use number: {text}"
        );
        assert!(
            text.contains("2. second"),
            "second item must use next number: {text}"
        );
    }
}
