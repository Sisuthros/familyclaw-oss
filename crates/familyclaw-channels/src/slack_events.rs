//! Slack Events API — signature verification and event → [`InboundMessage`].
//!
//! Wires up the missing half of [`crate::slack::SlackChannel`]: it was
//! outbound-only (`chat.postMessage`) with an `inject()` seam nobody called.
//! This module gives the gateway's `POST /slack/events` handler what it
//! needs to (1) prove a request really came from Slack (HMAC-SHA256 request
//! signing, Slack's documented scheme) and (2) turn a `message` event into
//! an [`InboundMessage`] ready for [`crate::slack::SlackChannel::inject`].
//!
//! Reference: <https://api.slack.com/authentication/verifying-requests-from-slack>

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::{ChannelError, ChannelResult};
use crate::message::InboundMessage;

type HmacSha256 = Hmac<Sha256>;

/// Slack's documented replay-protection freshness window (5 minutes).
const MAX_TIMESTAMP_SKEW_SECS: i64 = 300;

/// The `url_verification` handshake Slack sends once, when the Events API
/// subscription is first configured. The gateway must echo `challenge` back
/// verbatim as the response body.
pub const EVENT_TYPE_URL_VERIFICATION: &str = "url_verification";

/// Ordinary event delivery (the payload the gateway subscribes to at
/// runtime — `event.type == "message"` is the one this module extracts).
pub const EVENT_TYPE_CALLBACK: &str = "event_callback";

/// Verifies a Slack Events API request signature.
///
/// `signing_secret` — the app's Signing Secret (`SLACK_SIGNING_SECRET`).
/// `timestamp` — the raw `X-Slack-Request-Timestamp` header value.
/// `signature` — the raw `X-Slack-Signature` header value (`v0=<hex>`).
/// `body` — the request's raw body bytes (signature covers the exact bytes
/// Slack sent — this must run BEFORE any JSON parsing/re-serialization).
///
/// # Errors
/// [`ChannelError::InvalidInput`] if the timestamp is malformed, the
/// signature has the wrong shape, is outside the freshness window (replay
/// protection), or does not match.
pub fn verify_slack_signature(
    signing_secret: &str,
    timestamp: &str,
    signature: &str,
    body: &[u8],
) -> ChannelResult<()> {
    if signing_secret.trim().is_empty() {
        return Err(ChannelError::invalid_input(
            "SLACK_SIGNING_SECRET must not be empty",
        ));
    }
    let ts_secs: i64 = timestamp
        .trim()
        .parse()
        .map_err(|_| ChannelError::invalid_input("X-Slack-Request-Timestamp is not an integer"))?;
    let now_secs = chrono::Utc::now().timestamp();
    if (now_secs - ts_secs).abs() > MAX_TIMESTAMP_SKEW_SECS {
        return Err(ChannelError::invalid_input(
            "Slack request timestamp outside freshness window (possible replay)",
        ));
    }

    let sig_hex = signature.strip_prefix("v0=").ok_or_else(|| {
        ChannelError::invalid_input("X-Slack-Signature missing 'v0=' version prefix")
    })?;
    let expected_bytes = decode_hex(sig_hex)
        .map_err(|e| ChannelError::invalid_input(format!("invalid X-Slack-Signature hex: {e}")))?;

    let mut base = Vec::with_capacity(3 + timestamp.len() + 1 + body.len());
    base.extend_from_slice(b"v0:");
    base.extend_from_slice(timestamp.as_bytes());
    base.push(b':');
    base.extend_from_slice(body);

    let mut mac = HmacSha256::new_from_slice(signing_secret.as_bytes())
        .map_err(|e| ChannelError::invalid_input(format!("invalid signing secret: {e}")))?;
    mac.update(&base);

    mac.verify_slice(&expected_bytes)
        .map_err(|_| ChannelError::invalid_input("slack signature verification failed"))
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex string".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Parses a Slack Events API payload's `event_callback` body into an
/// [`InboundMessage`], or `Ok(None)` when the event is not an actionable
/// human `message` (bot echoes, edits/deletes, channel-topic subtypes, …).
///
/// `Ok(None)` (not an error) is the expected outcome for the majority of
/// event deliveries — the caller should respond `200 OK` either way (Slack
/// retries on non-2xx).
///
/// # Errors
/// [`ChannelError::InvalidInput`] if the payload's `event_callback` shape is
/// malformed (missing `event`).
pub fn parse_message_event(payload: &serde_json::Value) -> ChannelResult<Option<InboundMessage>> {
    let event = payload
        .get("event")
        .ok_or_else(|| ChannelError::invalid_input("event_callback payload missing 'event'"))?;

    let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if event_type != "message" {
        return Ok(None);
    }

    // Loop-prevention: bot-authored messages (including our own replies)
    // carry `bot_id` and/or `subtype == "bot_message"` — never re-inject
    // these, or the agent would talk to itself forever.
    if event.get("bot_id").is_some() {
        return Ok(None);
    }
    if let Some(subtype) = event.get("subtype").and_then(|v| v.as_str()) {
        // message_changed / message_deleted / channel_join / bot_message / …
        // — none of these are a new human utterance worth injecting.
        if subtype != "file_share" {
            return Ok(None);
        }
    }

    let user = event.get("user").and_then(|v| v.as_str()).unwrap_or("");
    let channel = event.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let text = event.get("text").and_then(|v| v.as_str()).unwrap_or("");

    if user.is_empty() || channel.is_empty() || text.is_empty() {
        return Ok(None);
    }

    Ok(Some(InboundMessage::new(user, channel, text)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &str, timestamp: &str, body: &str) -> String {
        let base = format!("v0:{timestamp}:{body}");
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(base.as_bytes());
        let bytes = mac.finalize().into_bytes();
        format!("v0={}", hex_encode(&bytes))
    }

    fn hex_encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn verifies_a_correctly_signed_request() {
        let secret = "shh-its-a-secret";
        let now = chrono::Utc::now().timestamp().to_string();
        let body = r#"{"type":"event_callback"}"#;
        let sig = sign(secret, &now, body);
        verify_slack_signature(secret, &now, &sig, body.as_bytes()).expect("valid signature");
    }

    #[test]
    fn rejects_tampered_body() {
        let secret = "shh-its-a-secret";
        let now = chrono::Utc::now().timestamp().to_string();
        let sig = sign(secret, &now, r#"{"type":"event_callback"}"#);
        let err =
            verify_slack_signature(secret, &now, &sig, br#"{"type":"tampered"}"#).unwrap_err();
        assert!(err.to_string().contains("verification failed"));
    }

    #[test]
    fn rejects_stale_timestamp() {
        let secret = "shh-its-a-secret";
        let stale = (chrono::Utc::now().timestamp() - 3600).to_string();
        let body = r#"{"type":"event_callback"}"#;
        let sig = sign(secret, &stale, body);
        let err = verify_slack_signature(secret, &stale, &sig, body.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("freshness window"));
    }

    #[test]
    fn rejects_missing_v0_prefix() {
        let secret = "shh";
        let now = chrono::Utc::now().timestamp().to_string();
        let err = verify_slack_signature(secret, &now, "deadbeef", b"{}").unwrap_err();
        assert!(err.to_string().contains("v0="));
    }

    #[test]
    fn url_verification_challenge_roundtrip() {
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"type":"url_verification","challenge":"abc123","token":"x"}"#,
        )
        .unwrap();
        assert_eq!(payload["type"], EVENT_TYPE_URL_VERIFICATION);
        assert_eq!(payload["challenge"], "abc123");
    }

    #[test]
    fn parses_a_plain_human_message() {
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"type":"event_callback","event":{"type":"message","user":"U1","channel":"C1","text":"hello"}}"#,
        )
        .unwrap();
        let msg = parse_message_event(&payload).unwrap().expect("some message");
        assert_eq!(msg.sender, "U1");
        assert_eq!(msg.conversation, "C1");
        assert_eq!(msg.body, "hello");
    }

    #[test]
    fn ignores_bot_authored_messages_to_prevent_loops() {
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"type":"event_callback","event":{"type":"message","user":"U1","channel":"C1","text":"hi","bot_id":"B1"}}"#,
        )
        .unwrap();
        assert!(parse_message_event(&payload).unwrap().is_none());
    }

    #[test]
    fn ignores_message_changed_subtype() {
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"type":"event_callback","event":{"type":"message","user":"U1","channel":"C1","text":"edited","subtype":"message_changed"}}"#,
        )
        .unwrap();
        assert!(parse_message_event(&payload).unwrap().is_none());
    }

    #[test]
    fn ignores_non_message_event_types() {
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"type":"event_callback","event":{"type":"reaction_added","user":"U1"}}"#,
        )
        .unwrap();
        assert!(parse_message_event(&payload).unwrap().is_none());
    }

    #[test]
    fn errors_on_missing_event_field() {
        let payload: serde_json::Value =
            serde_json::from_str(r#"{"type":"event_callback"}"#).unwrap();
        assert!(parse_message_event(&payload).is_err());
    }
}
