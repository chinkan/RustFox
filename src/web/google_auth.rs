#![allow(dead_code)]

use axum::response::sse::Event;
use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Sse},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::web::WebState;

const GOOGLE_WORKSPACE_SCOPES: &str = "https://www.googleapis.com/auth/drive \
     https://www.googleapis.com/auth/gmail.modify \
     https://www.googleapis.com/auth/calendar \
     https://www.googleapis.com/auth/documents \
     https://www.googleapis.com/auth/spreadsheets \
     https://www.googleapis.com/auth/presentations";

// ── Start ──────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct StartRequest {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Serialize)]
pub struct StartResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_url: String,
    expires_in: u64,
    interval: u64,
}

pub async fn start(
    State(_state): State<WebState>,
    Json(body): Json<StartRequest>,
) -> impl IntoResponse {
    if body.client_id.is_empty() || body.client_secret.is_empty() {
        return Json(StartResponse {
            error: Some("client_id and client_secret are required".into()),
            device_code: String::new(),
            user_code: String::new(),
            verification_url: String::new(),
            expires_in: 0,
            interval: 5,
        });
    }

    tracing::info!(
        client_id = %body.client_id,
        client_id_len = body.client_id.len(),
        scope = %GOOGLE_WORKSPACE_SCOPES,
        "google-auth/start: sending device code request"
    );

    let http = reqwest::Client::new();
    let resp = http
        .post("https://oauth2.googleapis.com/device/code")
        .form(&[
            ("client_id", &body.client_id),
            ("scope", &GOOGLE_WORKSPACE_SCOPES.to_string()),
        ])
        .send()
        .await;

    match resp {
        Err(e) => Json(StartResponse {
            error: Some(format!("Failed to contact Google: {e}")),
            device_code: String::new(),
            user_code: String::new(),
            verification_url: String::new(),
            expires_in: 0,
            interval: 5,
        }),
        Ok(r) if !r.status().is_success() => {
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            tracing::error!(%status, body = %text, "google-auth/start: Google returned error");
            Json(StartResponse {
                error: Some(format!("Google error: {text}")),
                device_code: String::new(),
                user_code: String::new(),
                verification_url: String::new(),
                expires_in: 0,
                interval: 5,
            })
        }
        Ok(r) => match r.json::<DeviceCodeResponse>().await {
            Err(e) => Json(StartResponse {
                error: Some(format!("Failed to parse Google response: {e}")),
                device_code: String::new(),
                user_code: String::new(),
                verification_url: String::new(),
                expires_in: 0,
                interval: 5,
            }),
            Ok(d) => Json(StartResponse {
                error: None,
                device_code: d.device_code,
                user_code: d.user_code,
                verification_url: d.verification_url,
                expires_in: d.expires_in,
                interval: d.interval,
            }),
        },
    }
}

// ── Poll SSE ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PollQuery {
    pub client_id: String,
    pub client_secret: String,
    pub interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenResponse {
    refresh_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

pub async fn poll(
    Path(device_code): Path<String>,
    Query(params): Query<PollQuery>,
    State(_state): State<WebState>,
) -> impl IntoResponse {
    let client_id = params.client_id.clone();
    let client_secret = params.client_secret.clone();
    let interval_secs = params.interval.unwrap_or(5).max(5);
    let poll_interval = std::time::Duration::from_secs(interval_secs);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1800); // 30 min max

    let stream = async_stream::stream! {
        let http = reqwest::Client::new();
        loop {
            tokio::time::sleep(poll_interval).await;

            if std::time::Instant::now() > deadline {
                yield Ok::<_, std::convert::Infallible>(
                    Event::default().event("error_msg").data("Authorization timed out.")
                );
                break;
            }

            let resp = http
                .post("https://oauth2.googleapis.com/token")
                .form(&[
                    ("client_id", client_id.as_str()),
                    ("client_secret", client_secret.as_str()),
                    ("device_code", device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await;

            let body: TokenResponse = match resp {
                Err(e) => {
                    yield Ok(Event::default().event("error_msg").data(format!("Network error: {e}")));
                    break;
                }
                Ok(r) => match r.json().await {
                    Err(e) => {
                        yield Ok(Event::default().event("error_msg").data(format!("Parse error: {e}")));
                        break;
                    }
                    Ok(b) => b,
                },
            };

            match body.error.as_deref() {
                None => {
                    match body.refresh_token {
                        Some(rt) => {
                            yield Ok(Event::default().event("token").data(rt));
                            break;
                        }
                        None => {
                            yield Ok(Event::default().event("error_msg").data(
                                "No refresh_token in response. Ensure the OAuth client type is 'Desktop app' (not 'TVs and Limited Input devices')."
                            ));
                            break;
                        }
                    }
                }
                Some("authorization_pending") => {
                    yield Ok(Event::default().event("pending").data(""));
                }
                Some("slow_down") => {
                    tokio::time::sleep(poll_interval).await;
                    yield Ok(Event::default().event("pending").data(""));
                }
                Some("access_denied") => {
                    yield Ok(Event::default().event("error_msg").data("Authorization was denied."));
                    break;
                }
                Some("expired_token") => {
                    yield Ok(Event::default().event("error_msg").data("Device code expired. Try again."));
                    break;
                }
                Some(other) => {
                    let desc = body.error_description.as_deref().unwrap_or("");
                    yield Ok(Event::default().event("error_msg").data(format!("{other}: {desc}")));
                    break;
                }
            }
        }
    };

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}
