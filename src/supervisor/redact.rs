//! Secret-redaction filter applied to artifact contents (and any other text
//! the supervisor might persist or echo back to the user).
//!
//! The patterns are intentionally simple — they match common credential-style
//! tokens (`api_key=...`, `Bearer ...`, `password: ...`, etc.) and replace
//! the *value* with `***`, preserving the original key and separator so the
//! redacted text remains readable.

use regex::Regex;
use std::sync::OnceLock;

static SECRET_RE: OnceLock<Regex> = OnceLock::new();

fn pattern() -> &'static Regex {
    SECRET_RE.get_or_init(|| {
        // $1 = key (api_key|password|secret|token|bearer)
        // $2 = separator (whitespace, ':', '=' — possibly empty)
        // value (\S+) is dropped and replaced with ***
        Regex::new(r"(?i)\b(api_key|password|secret|token|bearer)\b(\s*[:=]?\s*)\S+").unwrap()
    })
}

/// Replace credential-style values with `***`, preserving the key and
/// separator so the redacted text stays readable.
pub fn redact(s: &str) -> String {
    pattern().replace_all(s, "$1$2***").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_obvious_secrets_in_strings() {
        assert_eq!(redact("api_key=sk-abcdef123"), "api_key=***");
        assert_eq!(redact("Bearer xyz12345"), "Bearer ***");
        assert_eq!(redact("password: hunter2"), "password: ***");
        assert_eq!(redact("nothing sensitive"), "nothing sensitive");
    }
}
