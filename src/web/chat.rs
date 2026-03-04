#![allow(dead_code)]

use askama::Template;
use axum::{
    extract::{Path, State},
    response::{Html, IntoResponse, Sse},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::web::WebState;

#[derive(Template)]
#[template(path = "chat.html")]
struct ChatTemplate;

pub async fn page() -> impl IntoResponse {
    Html(ChatTemplate.render().expect("template render"))
}

#[derive(Deserialize)]
pub struct SendRequest {
    pub text: String,
}

#[derive(Serialize)]
pub struct SendResponse {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub async fn send(
    State(state): State<WebState>,
    Json(body): Json<SendRequest>,
) -> impl IntoResponse {
    let agent = match &state.agent {
        Some(a) => Arc::clone(a),
        None => {
            return Json(SendResponse {
                session_id: String::new(),
                error: Some("Bot not running — complete setup first".into()),
            });
        }
    };

    let text = body.text.trim().to_string();
    if text.is_empty() {
        return Json(SendResponse {
            session_id: String::new(),
            error: Some("empty message".into()),
        });
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    let sid = session_id.clone();

    tokio::spawn(async move {
        let incoming = crate::platform::IncomingMessage {
            platform: "web".into(),
            user_id: "web".into(),
            chat_id: "web".into(),
            user_name: "Web User".into(),
            text,
        };

        match agent.process_message(&incoming).await {
            Ok(response) => {
                for chunk in chunk_string(&response, 4) {
                    let _ = agent.web_tx.send((sid.clone(), chunk));
                    tokio::time::sleep(tokio::time::Duration::from_millis(8)).await;
                }
                let _ = agent.web_tx.send((sid.clone(), "\x00DONE".into()));
            }
            Err(e) => {
                let _ = agent.web_tx.send((sid.clone(), format!("\x00ERR:{e}")));
            }
        }
    });

    Json(SendResponse {
        session_id,
        error: None,
    })
}

pub async fn stream(
    Path(session_id): Path<String>,
    State(state): State<WebState>,
) -> impl IntoResponse {
    use axum::response::sse::Event;

    let maybe_rx = state.agent.as_ref().map(|a| a.web_tx.subscribe());
    let sid = session_id.clone();

    // Consume the broadcast stream and emit SSE events, stopping after a terminal event.
    let event_stream = async_stream::stream! {
        let rx = match maybe_rx {
            None => {
                yield Ok::<_, std::convert::Infallible>(Event::default().event("error").data("Bot not running"));
                return;
            }
            Some(r) => r,
        };
        let mut broadcast = BroadcastStream::new(rx);
        while let Some(result) = broadcast.next().await {
            let (id, token) = match result {
                Ok(pair) => pair,
                Err(_) => continue, // lagged — skip
            };
            if id != sid {
                continue;
            }
            if token == "\x00DONE" {
                yield Ok::<_, std::convert::Infallible>(Event::default().event("done").data(""));
                break;
            } else if let Some(msg) = token.strip_prefix("\x00ERR:") {
                yield Ok(Event::default().event("error").data(msg));
                break;
            } else {
                yield Ok(Event::default().event("token").data(token));
            }
        }
    };

    Sse::new(event_stream).keep_alive(axum::response::sse::KeepAlive::default())
}

fn chunk_string(s: &str, chars: usize) -> Vec<String> {
    s.chars()
        .collect::<Vec<_>>()
        .chunks(chars)
        .map(|c| c.iter().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_string_splits_evenly() {
        assert_eq!(chunk_string("abcdef", 2), vec!["ab", "cd", "ef"]);
    }

    #[test]
    fn chunk_string_handles_unicode() {
        assert_eq!(chunk_string("日本語", 1), vec!["日", "本", "語"]);
    }

    #[test]
    fn chunk_string_empty_input() {
        assert!(chunk_string("", 4).is_empty());
    }

    #[test]
    fn chunk_string_shorter_than_chunk_size() {
        assert_eq!(chunk_string("hi", 10), vec!["hi"]);
    }
}
