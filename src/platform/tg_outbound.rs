//! Outbound Telegram bubble index for ADR-0048 reply quotes (#358).
//!
//! Assistant text is already in SQLite `messages`, but that row has no Telegram
//! `message_id`. Bot API has no `getMessage`. After a user-visible send settles,
//! persist `(chat_id, message_id) → text` and, when
//! [`super::telegram_reply_to::quote_from_reply_parts`] would be
//! [`ReplyQuote::Unavailable`], resolve from this map before the locked
//! unavailable template.
//!
//! Teloxide 0.17 / teloxide-core 0.13 exposes outer `Message::quote()`
//! (`TextQuote`). It does not model `Message.rich_message`; serde drops that
//! field, so live updates pass `rich_message: None`. [`plain_text_from_rich_message`]
//! still runs when a caller has the JSON (tests, or a future raw update).

use anyhow::Result;
use rusqlite::OptionalExtension;
use serde_json::Value;

use crate::memory::MemoryStore;
use crate::platform::telegram_reply_to::{quote_from_reply_parts, ReplyQuote};

/// Extra sources tried only after legacy text/caption/media yield `Unavailable`.
///
/// Order: local outbound map, then outer `TextQuote`, then flattened
/// `rich_message` blocks.
pub struct ReplyFallbacks<'a> {
    pub local_text: Option<&'a str>,
    pub outer_quote: Option<&'a str>,
    /// `reply_to_message.rich_message` object when the caller still has it.
    pub rich_message: Option<&'a Value>,
}

/// Draft ids deleted before the formatted resend, plus the resend attempts.
pub struct SettledOutbound {
    pub deleted_draft_ids: Vec<i32>,
    pub resends: Vec<(Option<i32>, String)>,
}

/// Message ids that remain after delete/resend and should be indexed.
///
/// Drops failed sends (`None` id), empty bodies, and any id that was a deleted
/// streaming draft — even if a caller accidentally includes it in `resends`.
pub fn ids_to_index(settled: &SettledOutbound) -> Vec<(i32, String)> {
    let drafts: std::collections::HashSet<i32> =
        settled.deleted_draft_ids.iter().copied().collect();
    settled
        .resends
        .iter()
        .filter_map(|(id, text)| {
            let id = (*id)?;
            if text.is_empty() || drafts.contains(&id) {
                None
            } else {
                Some((id, text.clone()))
            }
        })
        .collect()
}

const RICH_WALK_MAX_DEPTH: u8 = 32;

/// Plain text of a Bot API `RichMessage` (`blocks`), for when legacy
/// `text` / `caption` are empty.
pub fn plain_text_from_rich_message(rich: &Value) -> Option<String> {
    let blocks = rich.get("blocks")?.as_array()?;
    let mut out = String::new();
    for block in blocks {
        append_rich_block(&mut out, block, 0);
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn append_rich_block(out: &mut String, block: &Value, depth: u8) {
    if depth > RICH_WALK_MAX_DEPTH {
        return;
    }
    let Some(obj) = block.as_object() else {
        return;
    };
    let mut piece = String::new();
    if let Some(text) = obj.get("text") {
        append_rich_text(&mut piece, text, depth + 1);
    }
    if let Some(expr) = obj.get("expression").and_then(Value::as_str) {
        push_line(&mut piece, expr);
    }
    if let Some(summary) = obj.get("summary") {
        let mut summary_text = String::new();
        append_rich_text(&mut summary_text, summary, depth + 1);
        push_line(&mut piece, summary_text.trim());
    }
    if let Some(blocks) = obj.get("blocks").and_then(Value::as_array) {
        for child in blocks {
            append_rich_block(&mut piece, child, depth + 1);
        }
    }
    if let Some(items) = obj.get("items").and_then(Value::as_array) {
        for item in items {
            if let Some(blocks) = item.get("blocks").and_then(Value::as_array) {
                for child in blocks {
                    append_rich_block(&mut piece, child, depth + 1);
                }
            }
        }
    }
    if let Some(rows) = obj.get("cells").and_then(Value::as_array) {
        for row in rows {
            let Some(cells) = row.as_array() else {
                continue;
            };
            let mut cells_text = Vec::new();
            for cell in cells {
                if let Some(text) = cell.get("text") {
                    let mut cell_text = String::new();
                    append_rich_text(&mut cell_text, text, depth + 1);
                    let cell_text = cell_text.trim();
                    if !cell_text.is_empty() {
                        cells_text.push(cell_text.to_string());
                    }
                }
            }
            if !cells_text.is_empty() {
                push_line(&mut piece, &cells_text.join(" | "));
            }
        }
    }
    if let Some(caption) = obj.get("caption") {
        let mut caption_text = String::new();
        if let Some(text) = caption.get("text") {
            append_rich_text(&mut caption_text, text, depth + 1);
        } else {
            append_rich_text(&mut caption_text, caption, depth + 1);
        }
        push_line(&mut piece, caption_text.trim());
    }
    push_line(out, piece.trim());
}

fn push_line(out: &mut String, line: &str) {
    if line.is_empty() {
        return;
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(line);
}

fn append_rich_text(out: &mut String, value: &Value, depth: u8) {
    if depth > RICH_WALK_MAX_DEPTH {
        return;
    }
    match value {
        Value::String(text) => out.push_str(text),
        Value::Array(items) => {
            for item in items {
                append_rich_text(out, item, depth + 1);
            }
        }
        Value::Object(map) => {
            if let Some(text) = map.get("text") {
                append_rich_text(out, text, depth + 1);
                return;
            }
            if let Some(alt) = map.get("alternative_text").and_then(Value::as_str) {
                out.push_str(alt);
                return;
            }
            if let Some(expr) = map.get("expression").and_then(Value::as_str) {
                out.push_str(expr);
                return;
            }
            for key in [
                "username",
                "hashtag",
                "cashtag",
                "bot_command",
                "email_address",
                "phone_number",
                "bank_card_number",
                "url",
            ] {
                if let Some(text) = map.get(key).and_then(Value::as_str) {
                    out.push_str(text);
                    return;
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn nonempty(text: Option<&str>) -> Option<&str> {
    text.filter(|s| !s.is_empty())
}

pub fn resolve_reply_quote(
    text: Option<&str>,
    caption: Option<&str>,
    has_media: bool,
    fallbacks: ReplyFallbacks<'_>,
) -> ReplyQuote {
    let base = quote_from_reply_parts(text, caption, has_media);
    if !matches!(base, ReplyQuote::Unavailable) {
        return base;
    }
    if let Some(local) = nonempty(fallbacks.local_text) {
        return ReplyQuote::Text(local.to_string());
    }
    if let Some(quoted) = nonempty(fallbacks.outer_quote) {
        return ReplyQuote::Text(quoted.to_string());
    }
    if let Some(rich) = fallbacks
        .rich_message
        .and_then(plain_text_from_rich_message)
    {
        return ReplyQuote::Text(rich);
    }
    ReplyQuote::Unavailable
}

pub async fn record(memory: &MemoryStore, chat_id: i64, message_id: i32, text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let conn = memory.connection();
    let conn = conn.lock().await;
    conn.execute(
        "INSERT INTO tg_outbound_text (chat_id, message_id, body)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(chat_id, message_id) DO UPDATE SET body = excluded.body",
        rusqlite::params![chat_id, message_id, text],
    )?;
    Ok(())
}

pub async fn record_finals(
    memory: &MemoryStore,
    chat_id: i64,
    finals: &[(i32, String)],
) -> Result<()> {
    for (message_id, text) in finals {
        record(memory, chat_id, *message_id, text).await?;
    }
    Ok(())
}

pub async fn lookup(memory: &MemoryStore, chat_id: i64, message_id: i32) -> Result<Option<String>> {
    let conn = memory.connection();
    let conn = conn.lock().await;
    let body = conn
        .query_row(
            "SELECT body FROM tg_outbound_text WHERE chat_id = ?1 AND message_id = ?2",
            rusqlite::params![chat_id, message_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::telegram_reply_to::compose_reply_to_user_text;
    use serde_json::json;

    fn fall<'a>(
        local_text: Option<&'a str>,
        outer_quote: Option<&'a str>,
        rich_message: Option<&'a Value>,
    ) -> ReplyFallbacks<'a> {
        ReplyFallbacks {
            local_text,
            outer_quote,
            rich_message,
        }
    }

    #[tokio::test]
    async fn record_then_lookup_returns_body() {
        let memory = MemoryStore::open_in_memory().unwrap();
        record(&memory, 111_001, 32886, "draft this in qwen")
            .await
            .unwrap();
        let got = lookup(&memory, 111_001, 32886).await.unwrap();
        assert_eq!(got.as_deref(), Some("draft this in qwen"));
    }

    #[tokio::test]
    async fn lookup_miss_and_other_chat_stay_empty() {
        let memory = MemoryStore::open_in_memory().unwrap();
        record(&memory, 7, 3, "only chat 7").await.unwrap();
        assert_eq!(lookup(&memory, 7, 99).await.unwrap(), None);
        assert_eq!(lookup(&memory, 8, 3).await.unwrap(), None);
    }

    #[tokio::test]
    async fn empty_body_is_not_stored() {
        let memory = MemoryStore::open_in_memory().unwrap();
        record(&memory, 7, 3, "").await.unwrap();
        assert_eq!(lookup(&memory, 7, 3).await.unwrap(), None);
    }

    #[tokio::test]
    async fn second_record_replaces_body() {
        let memory = MemoryStore::open_in_memory().unwrap();
        record(&memory, 7, 3, "first").await.unwrap();
        record(&memory, 7, 3, "edited").await.unwrap();
        assert_eq!(
            lookup(&memory, 7, 3).await.unwrap().as_deref(),
            Some("edited")
        );
    }

    #[tokio::test]
    async fn multi_chunk_maps_each_final_id_and_skips_deleted_drafts() {
        let settled = SettledOutbound {
            deleted_draft_ids: vec![10, 11],
            resends: vec![
                (Some(20), "alpha chunk".into()),
                (Some(21), "beta chunk".into()),
                (None, "send failed".into()),
                (Some(22), String::new()),
                (Some(10), "stale draft that was deleted".into()),
            ],
        };
        let finals = ids_to_index(&settled);
        assert_eq!(
            finals,
            vec![
                (20, "alpha chunk".to_string()),
                (21, "beta chunk".to_string()),
            ]
        );

        let memory = MemoryStore::open_in_memory().unwrap();
        record_finals(&memory, 42, &finals).await.unwrap();
        assert_eq!(
            lookup(&memory, 42, 20).await.unwrap().as_deref(),
            Some("alpha chunk")
        );
        assert_eq!(
            lookup(&memory, 42, 21).await.unwrap().as_deref(),
            Some("beta chunk")
        );
        assert_eq!(lookup(&memory, 42, 10).await.unwrap(), None);
        assert_eq!(lookup(&memory, 42, 11).await.unwrap(), None);
        assert_eq!(lookup(&memory, 42, 22).await.unwrap(), None);
    }

    #[test]
    fn miss_stays_unavailable_and_keeps_locked_template() {
        let quote = resolve_reply_quote(None, None, false, fall(None, None, None));
        assert_eq!(quote, ReplyQuote::Unavailable);
        let out = compose_reply_to_user_text("bot", 32886, &quote, "can you see my reply quote?");
        assert_eq!(
            out,
            "[Reply to bot msg 32886: unavailable]\n\ncan you see my reply quote?"
        );
    }

    #[test]
    fn local_map_hit_becomes_text_quote_with_locked_template() {
        let quote = resolve_reply_quote(
            None,
            Some(""),
            false,
            fall(Some("draft this in qwen"), None, None),
        );
        assert_eq!(quote, ReplyQuote::Text("draft this in qwen".into()));
        let out = compose_reply_to_user_text("bot", 32886, &quote, "can you see my reply quote?");
        assert_eq!(
            out,
            "[Reply to bot msg 32886]:\ndraft this in qwen\n\ncan you see my reply quote?"
        );
    }

    #[test]
    fn local_map_beats_outer_quote_and_rich_blocks() {
        let rich = json!({
            "blocks": [{ "type": "paragraph", "text": "from rich" }]
        });
        let quote = resolve_reply_quote(
            None,
            None,
            false,
            fall(Some("from map"), Some("from quote"), Some(&rich)),
        );
        assert_eq!(quote, ReplyQuote::Text("from map".into()));
    }

    #[test]
    fn outer_quote_used_when_map_misses() {
        let quote = resolve_reply_quote(
            None,
            None,
            false,
            fall(Some(""), Some("selected sentence"), None),
        );
        assert_eq!(quote, ReplyQuote::Text("selected sentence".into()));
    }

    #[test]
    fn rich_blocks_used_when_map_and_quote_miss() {
        let rich = json!({
            "blocks": [
                {
                    "type": "paragraph",
                    "text": [
                        { "type": "bold", "text": "Hello" },
                        " world"
                    ]
                },
                { "type": "heading", "text": "Title", "size": 1 }
            ]
        });
        let quote = resolve_reply_quote(None, None, false, fall(None, Some(""), Some(&rich)));
        assert_eq!(quote, ReplyQuote::Text("Hello world\nTitle".into()));
    }

    #[test]
    fn legacy_text_and_media_are_not_replaced_by_fallbacks() {
        let quote = resolve_reply_quote(
            Some("visible"),
            None,
            false,
            fall(Some("stored"), Some("quoted"), None),
        );
        assert_eq!(quote, ReplyQuote::Text("visible".into()));

        let caption =
            resolve_reply_quote(None, Some("cap"), true, fall(Some("stored"), None, None));
        assert_eq!(caption, ReplyQuote::Text("cap".into()));

        let media = resolve_reply_quote(None, None, true, fall(Some("stored"), Some("q"), None));
        assert_eq!(media, ReplyQuote::MediaOnly);
    }

    #[test]
    fn rich_list_and_empty_blocks() {
        let list = json!({
            "blocks": [{
                "type": "list",
                "items": [{
                    "label": "1.",
                    "blocks": [{ "type": "paragraph", "text": "alpha" }]
                }]
            }]
        });
        assert_eq!(
            plain_text_from_rich_message(&list).as_deref(),
            Some("alpha")
        );
        assert_eq!(plain_text_from_rich_message(&json!({ "blocks": [] })), None);
        assert_eq!(plain_text_from_rich_message(&json!({})), None);
    }
}
