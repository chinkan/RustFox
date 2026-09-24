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

use axum::extract::State;
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
        user_id: user.clone(),
        chat_id: user.clone(),
        user_name: user.clone(),
        text: text.clone(),
        attachments: vec![],
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
                let data = json!({ "message": e.to_string() }).to_string();
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
/// cancellation registry (same mechanism as Telegram /stop). Cancellation
/// takes effect at the next tool boundary inside the agent loop.
pub async fn cancel(State(state): State<PortalState>) -> Json<serde_json::Value> {
    let cancelled = state
        .agent
        .cancel_processing(state.config.user_name.clone())
        .await;
    Json(json!({ "cancelled": cancelled }))
}

/// GET /api/chat/history — messages of the active web conversation.
pub async fn history(
    State(state): State<PortalState>,
) -> Result<Json<serde_json::Value>, PortalError> {
    let user = state.config.user_name.clone();
    let conv_id = state
        .memory
        .get_or_create_conversation("web", &user)
        .await
        .map_err(PortalError::from)?;
    let messages = state
        .memory
        .load_messages(&conv_id)
        .await
        .map_err(PortalError::from)?;

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

    Ok(Json(
        json!({ "conversationId": conv_id, "messages": items }),
    ))
}

/// GET /api/chat/threads — one row for the active web conversation (MVP;
/// multi-thread browsing is a follow-up).
pub async fn threads(
    State(state): State<PortalState>,
) -> Result<Json<serde_json::Value>, PortalError> {
    let user = state.config.user_name.clone();
    let conv_id = state
        .memory
        .get_or_create_conversation("web", &user)
        .await
        .map_err(PortalError::from)?;
    let messages = state
        .memory
        .load_messages(&conv_id)
        .await
        .map_err(PortalError::from)?;
    let count = messages
        .iter()
        .filter(|m| {
            (m.role == "user" || m.role == "assistant")
                && m.content
                    .as_ref()
                    .map(|c| c.as_text())
                    .unwrap_or_default()
                    .trim()
                    != ""
        })
        .count();
    let title = messages
        .iter()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_ref().map(|c| c.as_text()))
        .map(|t| {
            let t = t.trim();
            if t.chars().count() > 60 {
                format!("{}…", t.chars().take(60).collect::<String>())
            } else {
                t.to_string()
            }
        })
        .unwrap_or_else(|| "New chat".to_string());

    Ok(Json(json!([{
        "id": conv_id,
        "title": title,
        "messageCount": count,
        "updatedAt": chrono::Utc::now().to_rfc3339(),
    }])))
}
