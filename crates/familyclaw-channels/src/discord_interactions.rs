//! Discord Interactions HTTP — signature verification and payload → message.
//!
//! MVP: a slash command's `message` option is parsed into an [`InboundMessage`].
//! Verification follows Discord's documentation: `timestamp || raw_body`.

use ed25519_dalek::{Signature, VerifyingKey};

use crate::error::{ChannelError, ChannelResult};
use crate::message::InboundMessage;

/// Discord interaction types (API v10).
const INTERACTION_PING: u8 = 1;
const INTERACTION_APPLICATION_COMMAND: u8 = 2;

/// Discord response types.
pub const RESPONSE_PONG: u8 = 1;
/// Discord response: send a channel message.
pub const RESPONSE_CHANNEL_MESSAGE: u8 = 4;
/// Discord response: deferred (the agent replies via webhook later).
pub const RESPONSE_DEFERRED_CHANNEL_MESSAGE: u8 = 5;

/// Verifies a Discord Interactions signature (Ed25519).
///
/// `public_key_hex` — 32 bytes in hex form (Developer Portal).
/// `signature_hex` — 64 bytes in hex (`X-Signature-Ed25519`).
/// `timestamp` — the raw string from the header (`X-Signature-Timestamp`).
/// `body` — the request's raw body for signature verification.
pub fn verify_signature(
    public_key_hex: &str,
    signature_hex: &str,
    timestamp: &str,
    body: &[u8],
) -> ChannelResult<()> {
    // Replay-protection freshness window (declared up-front to satisfy clippy's
    // items-after-statements). Discord guidance: ~5 min.
    const MAX_TIMESTAMP_SKEW_SECS: i64 = 300;

    let pk = decode_hex(public_key_hex)
        .map_err(|e| ChannelError::invalid_input(format!("invalid DISCORD_PUBLIC_KEY hex: {e}")))?;
    let sig_bytes = decode_hex(signature_hex).map_err(|e| {
        ChannelError::invalid_input(format!("invalid X-Signature-Ed25519 hex: {e}"))
    })?;
    let pk_arr: [u8; 32] = pk
        .try_into()
        .map_err(|_| ChannelError::invalid_input("DISCORD_PUBLIC_KEY must be 32 bytes"))?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| ChannelError::invalid_input("signature must be 64 bytes"))?;

    let verifying_key = VerifyingKey::from_bytes(&pk_arr)
        .map_err(|e| ChannelError::invalid_input(format!("invalid public key: {e}")))?;
    let signature = Signature::from_bytes(&sig_arr);

    let mut message = Vec::with_capacity(timestamp.len() + body.len());
    message.extend_from_slice(timestamp.as_bytes());
    message.extend_from_slice(body);

    verifying_key
        .verify_strict(&message, &signature)
        .map_err(|_| ChannelError::invalid_input("discord signature verification failed"))?;

    // Replay protection: a valid signature alone lets a captured request be
    // replayed forever. Reject requests whose (now-authenticated) timestamp is
    // outside the freshness window. Checked AFTER verify_strict so the timestamp
    // is proven authentic before we trust it.
    let ts_secs: i64 = timestamp.trim().parse().map_err(|_| {
        ChannelError::invalid_input("discord X-Signature-Timestamp is not a unix-seconds integer")
    })?;
    let now_secs = chrono::Utc::now().timestamp();
    if (now_secs - ts_secs).abs() > MAX_TIMESTAMP_SKEW_SECS {
        return Err(ChannelError::invalid_input(
            "discord interaction timestamp outside freshness window (possible replay)",
        ));
    }
    Ok(())
}

/// A parsed Discord interaction (MVP: slash commands).
#[derive(Debug, Clone)]
pub struct DiscordInteraction {
    /// Discord interaction type (1 = PING, 2 = `APPLICATION_COMMAND`, …).
    pub interaction_type: u8,
    /// The slash command's name, if type == 2.
    pub command_name: Option<String>,
    /// The value of the `message` option in the slash command.
    pub message_text: Option<String>,
    /// The user's snowflake (member.user.id or user.id).
    pub user_id: Option<String>,
    /// The channel's snowflake.
    pub channel_id: Option<String>,
}

impl DiscordInteraction {
    /// Deserializes an interaction from JSON.
    pub fn from_payload(payload: &serde_json::Value) -> ChannelResult<Self> {
        let interaction_type = u8::try_from(
            payload
                .get("type")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| ChannelError::invalid_input("interaction missing type"))?,
        )
        .map_err(|_| ChannelError::invalid_input("interaction type out of range"))?;

        let data = payload.get("data");
        let command_name = data
            .and_then(|d| d.get("name"))
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let message_text = data.and_then(extract_message_option);

        let user_id = payload
            .pointer("/member/user/id")
            .or_else(|| payload.pointer("/user/id"))
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let channel_id = payload
            .get("channel_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        Ok(Self {
            interaction_type,
            command_name,
            message_text,
            user_id,
            channel_id,
        })
    }

    /// Converts the slash command into an [`InboundMessage`].
    ///
    /// Uses `user_id` as the sender and `channel_id` or `"discord"` as the conversation.
    pub fn into_inbound(self) -> ChannelResult<InboundMessage> {
        let body = self
            .message_text
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ChannelError::invalid_input("slash command message option empty"))?;
        let sender = self.user_id.unwrap_or_else(|| "discord-user".to_string());
        let conversation = self.channel_id.unwrap_or_else(|| "discord".to_string());
        InboundMessage::new(sender, conversation, body)
    }

    /// Is this a Discord PING (type 1)?
    #[must_use]
    pub fn is_ping(&self) -> bool {
        self.interaction_type == INTERACTION_PING
    }

    /// Is this a slash command (type 2)?
    #[must_use]
    pub fn is_application_command(&self) -> bool {
        self.interaction_type == INTERACTION_APPLICATION_COMMAND
    }
}

fn extract_message_option(data: &serde_json::Value) -> Option<String> {
    data.get("options")?.as_array()?.iter().find_map(|opt| {
        let name = opt.get("name")?.as_str()?;
        if name == "message" {
            opt.get("value")?.as_str().map(str::to_string)
        } else {
            None
        }
    })
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Err("odd hex length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("byte at {i}: {e}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_application_command_message_option() {
        let json: serde_json::Value = serde_json::json!({
            "type": 2,
            "channel_id": "123",
            "data": {
                "name": "familyclaw",
                "options": [{ "name": "message", "type": 3, "value": "Hei perhe" }]
            },
            "member": { "user": { "id": "user-42" } }
        });
        let ix = DiscordInteraction::from_payload(&json).expect("parse");
        assert!(ix.is_application_command());
        assert_eq!(ix.command_name.as_deref(), Some("familyclaw"));
        let inbound = ix.into_inbound().expect("inbound");
        assert_eq!(inbound.body, "Hei perhe");
        assert_eq!(inbound.sender, "user-42");
        assert_eq!(inbound.conversation, "123");
    }

    #[test]
    fn ping_is_recognized() {
        let json = serde_json::json!({ "type": 1 });
        let ix = DiscordInteraction::from_payload(&json).expect("parse");
        assert!(ix.is_ping());
    }

    #[test]
    fn verify_rejects_bad_signature() {
        let pk = "a".repeat(64);
        let sig = "b".repeat(128);
        let err = verify_signature(&pk, &sig, "123", b"{}").unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    /// A small hex encoder for test data (the inverse of `decode_hex`).
    fn encode_hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    #[test]
    fn verify_accepts_valid_signature() {
        use ed25519_dalek::{Signer, SigningKey};

        // A deterministic key for testing (NOT a production secret).
        let signing_key = SigningKey::from_bytes(&[0u8; 32]);
        let verifying_key = signing_key.verifying_key();

        // FRESH timestamp: replay protection requires the timestamp to be
        // within the freshness window, so we use the current moment (a fixed
        // past value would be rejected).
        let timestamp = chrono::Utc::now().timestamp().to_string();
        let body = br#"{"type":1}"#;

        // Discord signs `timestamp || raw_body`.
        let mut message = Vec::new();
        message.extend_from_slice(timestamp.as_bytes());
        message.extend_from_slice(body);
        let signature = signing_key.sign(&message);

        let pk_hex = encode_hex(verifying_key.as_bytes());
        let sig_hex = encode_hex(&signature.to_bytes());

        let result = verify_signature(&pk_hex, &sig_hex, &timestamp, body);
        assert!(
            result.is_ok(),
            "valid fresh signature must verify: {result:?}"
        );
    }

    #[test]
    fn verify_rejects_stale_timestamp_replay() {
        // FIX-4 regression: a perfectly-signed request with an OLD timestamp must
        // be rejected (replay protection), even though the Ed25519 signature is valid.
        use ed25519_dalek::{Signer, SigningKey};

        let signing_key = SigningKey::from_bytes(&[0u8; 32]);
        let verifying_key = signing_key.verifying_key();

        // 1 hour in the past — well outside the 5-min freshness window.
        let stale_ts = (chrono::Utc::now().timestamp() - 3600).to_string();
        let body = br#"{"type":1}"#;

        let mut message = Vec::new();
        message.extend_from_slice(stale_ts.as_bytes());
        message.extend_from_slice(body);
        let signature = signing_key.sign(&message); // genuinely valid signature

        let pk_hex = encode_hex(verifying_key.as_bytes());
        let sig_hex = encode_hex(&signature.to_bytes());

        let err = verify_signature(&pk_hex, &sig_hex, &stale_ts, body)
            .expect_err("stale-timestamp replay must be rejected even with a valid signature");
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn verify_rejects_tampered_body_with_valid_key() {
        use ed25519_dalek::{Signer, SigningKey};

        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let verifying_key = signing_key.verifying_key();

        let timestamp = "1700000001";
        let signed_body = br#"{"type":2}"#;

        let mut message = Vec::new();
        message.extend_from_slice(timestamp.as_bytes());
        message.extend_from_slice(signed_body);
        let signature = signing_key.sign(&message);

        let pk_hex = encode_hex(verifying_key.as_bytes());
        let sig_hex = encode_hex(&signature.to_bytes());

        // A different body than what was signed → verification fails even though the key matches.
        let tampered_body = br#"{"type":3}"#;
        let err = verify_signature(&pk_hex, &sig_hex, timestamp, tampered_body).unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn verify_rejects_odd_length_hex() {
        // Odd hex length → decode_hex returns an error (InvalidInput).
        let err = verify_signature("abc", &"b".repeat(128), "1", b"{}").unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn verify_rejects_wrong_length_public_key() {
        // Valid hex but the wrong byte count (16 bytes instead of 32) → the 32-byte try_into fails.
        let short_pk = "a".repeat(32); // 16 bytes
        let err = verify_signature(&short_pk, &"b".repeat(128), "1", b"{}").unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn verify_rejects_wrong_length_signature() {
        // A valid 32-byte key but a signature that's too short (32 bytes instead of 64).
        let pk = "a".repeat(64);
        let short_sig = "b".repeat(64); // 32 bytes
        let err = verify_signature(&pk, &short_sig, "1", b"{}").unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn from_payload_missing_type_errors() {
        // No "type" field → InvalidInput ("interaction missing type").
        let json = serde_json::json!({ "data": { "name": "x" } });
        let err = DiscordInteraction::from_payload(&json).unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn from_payload_type_out_of_range_errors() {
        // type > u8::MAX → u8::try_from fails ("interaction type out of range").
        let json = serde_json::json!({ "type": 300 });
        let err = DiscordInteraction::from_payload(&json).unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn from_payload_non_numeric_type_errors() {
        // type is not a number → as_u64() returns None → InvalidInput.
        let json = serde_json::json!({ "type": "ping" });
        let err = DiscordInteraction::from_payload(&json).unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn from_payload_without_data_yields_none_fields() {
        // Only type, no data and no user/channel → all options are None.
        let json = serde_json::json!({ "type": 2 });
        let ix = DiscordInteraction::from_payload(&json).expect("parse");
        assert!(ix.is_application_command());
        assert_eq!(ix.command_name, None);
        assert_eq!(ix.message_text, None);
        assert_eq!(ix.user_id, None);
        assert_eq!(ix.channel_id, None);
    }

    #[test]
    fn from_payload_falls_back_to_top_level_user_id() {
        // member.user.id is missing → falls back to user.id (DM context).
        let json = serde_json::json!({
            "type": 2,
            "user": { "id": "dm-user-9" }
        });
        let ix = DiscordInteraction::from_payload(&json).expect("parse");
        assert_eq!(ix.user_id.as_deref(), Some("dm-user-9"));
    }

    #[test]
    fn into_inbound_uses_defaults_when_ids_missing() {
        // user_id/channel_id are missing → defaults are "discord-user" and "discord".
        let json = serde_json::json!({
            "type": 2,
            "data": {
                "name": "fc",
                "options": [{ "name": "message", "type": 3, "value": "hei" }]
            }
        });
        let ix = DiscordInteraction::from_payload(&json).expect("parse");
        let inbound = ix.into_inbound().expect("inbound");
        assert_eq!(inbound.sender, "discord-user");
        assert_eq!(inbound.conversation, "discord");
        assert_eq!(inbound.body, "hei");
    }

    #[test]
    fn into_inbound_empty_message_errors() {
        // The message option is pure whitespace → filter drops it → InvalidInput.
        let json = serde_json::json!({
            "type": 2,
            "data": {
                "name": "fc",
                "options": [{ "name": "message", "type": 3, "value": "   " }]
            }
        });
        let ix = DiscordInteraction::from_payload(&json).expect("parse");
        let err = ix.into_inbound().unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn into_inbound_no_message_option_errors() {
        // No message_text at all → into_inbound gives InvalidInput.
        let json = serde_json::json!({ "type": 2, "data": { "name": "fc" } });
        let ix = DiscordInteraction::from_payload(&json).expect("parse");
        assert_eq!(ix.message_text, None);
        let err = ix.into_inbound().unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn extract_message_option_ignores_non_message_options() {
        // Only an option named "channel" → message_text remains None.
        let json = serde_json::json!({
            "type": 2,
            "data": {
                "name": "fc",
                "options": [{ "name": "channel", "type": 3, "value": "yleinen" }]
            }
        });
        let ix = DiscordInteraction::from_payload(&json).expect("parse");
        assert_eq!(ix.message_text, None);
    }

    #[test]
    fn extract_message_option_skips_non_string_value() {
        // The message option's value is not a string (a number) → as_str() is None → not extracted.
        let json = serde_json::json!({
            "type": 2,
            "data": {
                "name": "fc",
                "options": [{ "name": "message", "type": 4, "value": 42 }]
            }
        });
        let ix = DiscordInteraction::from_payload(&json).expect("parse");
        assert_eq!(ix.message_text, None);
    }

    #[test]
    fn extract_message_option_picks_message_among_many() {
        // Multiple options; message is correctly picked out among the others.
        let json = serde_json::json!({
            "type": 2,
            "data": {
                "name": "fc",
                "options": [
                    { "name": "channel", "type": 3, "value": "yleinen" },
                    { "name": "message", "type": 3, "value": "löytyi" }
                ]
            }
        });
        let ix = DiscordInteraction::from_payload(&json).expect("parse");
        assert_eq!(ix.message_text.as_deref(), Some("löytyi"));
    }
}
