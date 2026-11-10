//! Portal chat: SSE streaming + history (ADR 0005, docs/portal-api.md).
//!
//! Chat answers reuse `Agent::process_message` with its token/tool event
//! channels bridged into an SSE stream. The web identity (`platform="web"`)
//! keeps browser conversations separate from Telegram while sharing the one
//! runtime, prompt, and memory store.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::Json;
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;

use crate::platform::tool_notifier::ToolEvent;
use crate::platform::IncomingMessage;
use crate::tool_registry::ToolUiMode;

use super::error::PortalError;
use super::PortalState;

#[derive(Deserialize)]
pub struct SendRequest {
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryQuery {
    #[serde(default)]
    conversation_id: Option<String>,
}

/// Minimal `Stream` over an mpsc receiver, required by `Sse`.
struct EventStream {
    rx: mpsc::Receiver<Event>,
}

impl Stream for EventStream {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|opt| opt.map(Ok))
    }
}

/// POST /api/chat — SSE stream of the agent's answer.
/// One generation per portal identity (ADR 0005): concurrent sends → 409.
pub async fn send(
    State(state): State<PortalState>,
    Json(req): Json<SendRequest>,
) -> Result<impl IntoResponse, PortalError> {
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return Err(PortalError::bad_request(
            "empty_message",
            "Message text is required",
        ));
    }
    if state
        .chat_busy
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(PortalError::conflict(
            "chat_in_progress",
            "A generation is already running",
        ));
    }

    let user = state.config.user_name.clone();
    let incoming = IncomingMessage {
        platform: "web".to_string(),
        bot_id: crate::platform::DEFAULT_BOT_ID.to_string(),
        user_id: user.clone(),
        chat_id: user.clone(),
        user_name: user.clone(),
        text: text.clone(),
        attachments: vec![],
        schedule_id: None,
        device_id: None,
        device_room: None,
        device_fw: None,
        voice_origin: false,
        voice_spoken_lang: None,
    };

    // Bridge: agent output channels → single SSE event channel.
    let (token_tx, mut token_rx) = mpsc::channel::<String>(128);
    let (tool_tx, mut tool_rx) = mpsc::channel::<ToolEvent>(64);
    let (evt_tx, evt_rx) = mpsc::channel::<Event>(256);

    let agent = state.agent.clone();
    let busy_flag: std::sync::Arc<AtomicBool> = state.chat_busy.clone();

    let runner = tokio::spawn(async move {
        // Busy flag released on any exit (panic-safe via Drop).
        struct BusyGuard(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for BusyGuard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _busy = BusyGuard(busy_flag);

        let evt = evt_tx.clone();
        let token_relay = tokio::spawn(async move {
            while let Some(tok) = token_rx.recv().await {
                let data = json!({ "delta": tok }).to_string();
                if evt
                    .send(Event::default().event("token").data(data))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let evt = evt_tx.clone();
        let tool_relay = tokio::spawn(async move {
            while let Some(te) = tool_rx.recv().await {
                let data = match &te {
                    ToolEvent::Started { name, .. } => {
                        json!({ "name": name, "status": "started" })
                    }
                    ToolEvent::Completed { name, success } => {
                        json!({ "name": name, "status": "completed", "success": success })
                    }
                    ToolEvent::Finished { success } => {
                        json!({ "status": "finished", "success": success })
                    }
                }
                .to_string();
                if evt
                    .send(Event::default().event("tool").data(data))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        let result = agent
            .process_message(incoming, Some(tool_tx), Some(token_tx), ToolUiMode::Verbose)
            .await;

        // process_message consumed/dropped the senders → relays see EOF now.
        let _ = tokio::join!(token_relay, tool_relay);

        match result {
            Ok(answer) => {
                let data = json!({ "content": answer }).to_string();
                let _ = evt_tx.send(Event::default().event("done").data(data)).await;
            }
            Err(e) => {
                let data = json!({ "message": crate::llm::user_facing_llm_error(&e) }).to_string();
                let _ = evt_tx
                    .send(Event::default().event("error").data(data))
                    .await;
            }
        }
    });
    // If the whole runtime shuts down mid-run, at least log it.
    tokio::spawn(async move {
        if runner.await.is_err() {
            tracing::warn!("Portal chat runner task panicked");
        }
    });

    Ok(Sse::new(EventStream { rx: evt_rx }).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("ping"),
    ))
}

/// POST /api/chat/cancel — cancel the active generation via the agent's own
/// cancellation registry (same mechanism as Telegram /stop) and any in-flight
/// supervisor tasks for session `default:{user_name}`. Agent-loop cancel takes
/// effect at the next tool boundary; supervisor cancel reaches `Backend::cancel`.
pub async fn cancel(State(state): State<PortalState>) -> Json<serde_json::Value> {
    let cancelled = state
        .agent
        .cancel_processing(state.config.user_name.clone())
        .await;
    Json(json!({ "cancelled": cancelled }))
}

/// GET /api/chat/history — active web conversation, or one owned web thread.
///
/// No query get-or-creates the active conversation (today's single-thread
/// path). `?conversationId=` loads that web row for this portal user,
/// including archived threads, and does not create or unarchive.
pub async fn history(
    State(state): State<PortalState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<serde_json::Value>, PortalError> {
    let user = state.config.user_name.clone();
    let requested = query
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());

    let (conv_id, messages) = if let Some(id) = requested {
        let owned = state
            .memory
            .find_owned_web_conversation(id, crate::platform::DEFAULT_BOT_ID, &user)
            .await
            .map_err(PortalError::from)?;
        if owned.is_none() {
            return Err(PortalError::new(
                StatusCode::NOT_FOUND,
                "conversation_not_found",
                "Conversation not found",
            ));
        }
        let messages = state
            .memory
            .load_messages_including_archived(id)
            .await
            .map_err(PortalError::from)?;
        (id.to_string(), messages)
    } else {
        let conv_id = state
            .memory
            .get_or_create_conversation("web", crate::platform::DEFAULT_BOT_ID, &user)
            .await
            .map_err(PortalError::from)?;
        let messages = state
            .memory
            .load_messages(&conv_id)
            .await
            .map_err(PortalError::from)?;
        (conv_id, messages)
    };

    Ok(Json(history_body(&conv_id, &messages)))
}

/// GET /api/chat/threads — active web conversation, then archived ones.
///
/// Still get-or-creates the active row so a first visit has a thread. Archived
/// titles are derived from stored messages, not `load_messages` (that helper
/// returns nothing for an archived id). `updatedAt` is the stored timestamp.
pub async fn threads(
    State(state): State<PortalState>,
) -> Result<Json<serde_json::Value>, PortalError> {
    let user = state.config.user_name.clone();
    state
        .memory
        .get_or_create_conversation("web", crate::platform::DEFAULT_BOT_ID, &user)
        .await
        .map_err(PortalError::from)?;
    let rows = state
        .memory
        .list_web_conversations(crate::platform::DEFAULT_BOT_ID, &user)
        .await
        .map_err(PortalError::from)?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "title": row.title,
                "messageCount": row.message_count,
                "updatedAt": stored_updated_at(&row.updated_at),
                "archived": row.archived,
            })
        })
        .collect();
    Ok(Json(json!(items)))
}

/// POST /api/chat/threads — archive the active web thread and open a fresh one.
///
/// Empty body or `{}`. A generation in progress is `409` and does not archive.
/// An active thread with no visible messages is returned unchanged.
pub async fn create_thread(
    State(state): State<PortalState>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, PortalError> {
    if !empty_json_body(&body)? {
        return Err(PortalError::bad_request(
            "invalid_body",
            "Request body must be empty",
        ));
    }
    if state.chat_busy.load(Ordering::SeqCst) {
        return Err(PortalError::conflict(
            "chat_in_progress",
            "A generation is already running",
        ));
    }

    let user = state.config.user_name.clone();
    let fresh = state
        .memory
        .start_fresh_web_conversation(crate::platform::DEFAULT_BOT_ID, &user)
        .await
        .map_err(PortalError::from)?;
    if fresh.created {
        // Same mid-run reset as Agent::clear_conversation, scoped to the
        // portal user. Never passes platform "telegram".
        state.agent.reset_web_mid_run_mode(user).await;
    }

    Ok(Json(json!({
        "id": fresh.id,
        "title": fresh.title,
        "messageCount": fresh.message_count,
        "updatedAt": stored_updated_at(&fresh.updated_at),
        "created": fresh.created,
        "previousId": fresh.previous_id,
    })))
}

fn empty_json_body(body: &[u8]) -> Result<bool, PortalError> {
    if std::str::from_utf8(body).unwrap_or("").trim().is_empty() {
        return Ok(true);
    }
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(value) if value.as_object().is_some_and(|obj| obj.is_empty()) => Ok(true),
        Ok(_) => Ok(false),
        Err(_) => Err(PortalError::bad_request(
            "invalid_body",
            "Expected a JSON object",
        )),
    }
}

fn history_body(conv_id: &str, messages: &[crate::llm::ChatMessage]) -> serde_json::Value {
    let mut items = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        if m.role != "user" && m.role != "assistant" {
            continue;
        }
        let content = m.content.as_ref().map(|c| c.as_text()).unwrap_or_default();
        if content.trim().is_empty() {
            continue; // tool-call-only assistant rows
        }
        items.push(json!({
            "id": format!("h{i}"),
            "role": m.role,
            "content": content,
            "createdAt": null,
        }));
    }
    json!({ "conversationId": conv_id, "messages": items })
}

/// SQLite `datetime('now')` (`YYYY-MM-DD HH:MM:SS`, UTC) as RFC3339.
/// A value that is already RFC3339 is returned unchanged.
fn stored_updated_at(stored: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(stored, "%Y-%m-%d %H:%M:%S")
        .map(|naive| {
            naive
                .and_utc()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
        .unwrap_or_else(|_| stored.to_string())
}
