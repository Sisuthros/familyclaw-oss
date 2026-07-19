//! Redaction of secrets in log messages (Layer A).

use familyclaw_actions::proof::redact_free_text;

/// Redacts a string before it is logged.
///
/// Uses [`familyclaw_actions::proof::redact_free_text`] — the same heuristic
/// used for proof bundles.
#[must_use]
pub fn redact_for_log(message: &str) -> String {
    redact_free_text(message).0
}

/// Builds a log message from a command line, redacting parts that look like secrets.
#[must_use]
pub fn redact_command_line(parts: &[String]) -> String {
    let joined = parts.join(" ");
    redact_for_log(&joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_masks_sk_like_token() {
        let secret = format!("sk-{}", "live".repeat(4));
        let msg = format!("cmd --token {secret}");
        let redacted = redact_for_log(&msg);
        assert!(!redacted.contains(&secret));
    }

    #[test]
    fn innocent_text_unchanged() {
        let msg = "mock_server --port 8080";
        assert_eq!(redact_for_log(msg), msg);
    }
}
