//! Telegram notify helpers for pending secret requests (Slice 2).
//!
//! The human-readable body never includes the secret *value* or the raw claim
//! token. The claim URL may carry an opaque id in its path — callers concatenate
//! body + URL when sending to Telegram.

/// Human-readable Telegram body (no claim token, no secret value).
pub fn secret_request_notify_text(name: &str) -> String {
    format!(
        "🔐 RustFox needs secret `{name}`.\n\
         Open the portal link to enter it (masked). Never paste secrets in chat."
    )
}

/// Build a portal claim URL. `portal_base` is e.g. `http://127.0.0.1:8090/`.
/// The opaque claim token is a path segment only.
pub fn secret_request_claim_url(portal_base: &str, claim_token: &str) -> String {
    let base = portal_base.trim_end_matches('/');
    format!("{base}/secrets/claim/{claim_token}")
}

/// Full Telegram message: body + claim URL on its own line.
pub fn format_secret_request_notify(name: &str, claim_url: &str) -> String {
    format!("{}\n{}", secret_request_notify_text(name), claim_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_message_contains_name_not_value_or_raw_token() {
        let name = "OPENROUTER_API_KEY";
        let secret_value = "sk-live-NEVER-IN-TELEGRAM-msg";
        let claim_token = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
        let url = secret_request_claim_url("http://127.0.0.1:8090/", claim_token);
        let body = secret_request_notify_text(name);
        let full = format_secret_request_notify(name, &url);

        assert!(body.contains(name));
        assert!(!body.contains(secret_value));
        assert!(
            !body.contains(claim_token),
            "body must not echo the raw claim id"
        );
        assert!(!body.to_lowercase().contains("token="));
        assert!(!body.to_lowercase().contains("token:"));

        assert!(full.contains(name));
        assert!(!full.contains(secret_value));
        // Opaque id may appear only inside the URL path.
        assert!(full.contains(&url));
        assert!(url.contains(claim_token));
        assert!(!full.contains(&format!("token={claim_token}")));
        assert!(!full.contains(&format!("Token: {claim_token}")));
    }
}
