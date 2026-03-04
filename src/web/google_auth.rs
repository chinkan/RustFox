#![allow(dead_code)]

use axum::{
    extract::{Query, State},
    response::{Html, IntoResponse},
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

// ── OAuth session state (shared via WebState) ───────────────────────────────

#[derive(Default)]
pub struct OAuthSession {
    pub state_token: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub result: Option<OAuthResult>,
}

pub enum OAuthResult {
    Token(String),
    Error(String),
}

// ── Start: generate authorization URL ──────────────────────────────────────

#[derive(Deserialize)]
pub struct StartRequest {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

#[derive(Serialize)]
pub struct StartResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub auth_url: String,
}

pub async fn start(
    State(state): State<WebState>,
    Json(body): Json<StartRequest>,
) -> impl IntoResponse {
    if body.client_id.is_empty() || body.client_secret.is_empty() {
        return Json(StartResponse {
            error: Some("client_id and client_secret are required".into()),
            auth_url: String::new(),
        });
    }

    let state_token = uuid::Uuid::new_v4().to_string();

    {
        let mut session = state.oauth_session.lock().await;
        *session = OAuthSession {
            state_token: state_token.clone(),
            client_id: body.client_id.clone(),
            client_secret: body.client_secret.clone(),
            redirect_uri: body.redirect_uri.clone(),
            result: None,
        };
    }

    let mut url = reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
        .expect("static URL is valid");
    url.query_pairs_mut()
        .append_pair("client_id", &body.client_id)
        .append_pair("redirect_uri", &body.redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", GOOGLE_WORKSPACE_SCOPES)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", &state_token);

    tracing::info!(
        client_id = %body.client_id,
        redirect_uri = %body.redirect_uri,
        "google-auth/start: generated authorization URL"
    );

    Json(StartResponse {
        error: None,
        auth_url: url.to_string(),
    })
}

// ── Callback: receive authorization code from Google ───────────────────────

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

const CALLBACK_SUCCESS_HTML: &str = r#"<!DOCTYPE html>
<html><head><title>Google Authorization</title>
<style>body{font-family:sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#0d1117;color:#3fb950;font-size:1.1rem;text-align:center}</style>
</head><body><p>Google Workspace connected.<br>You can close this tab and return to the config page.</p></body></html>"#;

const CALLBACK_ERROR_HTML: &str = r#"<!DOCTYPE html>
<html><head><title>Google Authorization</title>
<style>body{font-family:sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#0d1117;color:#f85149;font-size:1.1rem;text-align:center}</style>
</head><body><p>Authorization failed.<br>Close this tab and check the config page for details.</p></body></html>"#;

pub async fn callback(
    Query(params): Query<CallbackQuery>,
    State(state): State<WebState>,
) -> impl IntoResponse {
    if let Some(err) = params.error {
        let mut session = state.oauth_session.lock().await;
        session.result = Some(OAuthResult::Error(format!("Authorization denied: {err}")));
        return Html(CALLBACK_ERROR_HTML.to_string());
    }

    let code = match params.code {
        Some(c) => c,
        None => {
            let mut session = state.oauth_session.lock().await;
            session.result = Some(OAuthResult::Error(
                "No authorization code received from Google".into(),
            ));
            return Html(CALLBACK_ERROR_HTML.to_string());
        }
    };

    let (client_id, client_secret, redirect_uri, expected_state) = {
        let session = state.oauth_session.lock().await;
        (
            session.client_id.clone(),
            session.client_secret.clone(),
            session.redirect_uri.clone(),
            session.state_token.clone(),
        )
    };

    if params.state.as_deref() != Some(&expected_state) {
        let mut session = state.oauth_session.lock().await;
        session.result = Some(OAuthResult::Error("State mismatch — possible CSRF".into()));
        return Html(CALLBACK_ERROR_HTML.to_string());
    }

    let http = reqwest::Client::new();
    let resp = http
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("code", code.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await;

    let result = match resp {
        Err(e) => OAuthResult::Error(format!("Network error: {e}")),
        Ok(r) => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            if let Some(token) = body.get("refresh_token").and_then(|v| v.as_str()) {
                tracing::info!("google-auth/callback: got refresh_token");
                OAuthResult::Token(token.to_string())
            } else {
                let err = body
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown_error");
                let desc = body
                    .get("error_description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                tracing::error!(%err, %desc, "google-auth/callback: token exchange failed");
                OAuthResult::Error(format!("{err}: {desc}"))
            }
        }
    };

    let html = match &result {
        OAuthResult::Token(_) => CALLBACK_SUCCESS_HTML,
        OAuthResult::Error(_) => CALLBACK_ERROR_HTML,
    };

    {
        let mut session = state.oauth_session.lock().await;
        session.result = Some(result);
    }

    Html(html.to_string())
}

// ── Status: UI polls this to retrieve the result ────────────────────────────

#[derive(Serialize)]
pub struct StatusResponse {
    pub status: &'static str, // "pending" | "success" | "error"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub async fn status(State(state): State<WebState>) -> impl IntoResponse {
    let session = state.oauth_session.lock().await;
    match &session.result {
        None => Json(StatusResponse {
            status: "pending",
            token: None,
            error: None,
        }),
        Some(OAuthResult::Token(t)) => Json(StatusResponse {
            status: "success",
            token: Some(t.clone()),
            error: None,
        }),
        Some(OAuthResult::Error(e)) => Json(StatusResponse {
            status: "error",
            token: None,
            error: Some(e.clone()),
        }),
    }
}
