//! Discord Interactions HTTP — allekirjoituksen tarkistus ja payload → viesti.
//!
//! MVP: slash-komennon `message`-option parsitaan [`InboundMessage`]:ksi.
//! Verify noudattaa Discord-dokumentaatiota: `timestamp || raw_body`.

use ed25519_dalek::{Signature, VerifyingKey};

use crate::error::{ChannelError, ChannelResult};
use crate::message::InboundMessage;

/// Discord interaction -tyypit (API v10).
const INTERACTION_PING: u8 = 1;
const INTERACTION_APPLICATION_COMMAND: u8 = 2;

/// Discord-vastauksen tyypit.
pub const RESPONSE_PONG: u8 = 1;
/// Discord-vastaus: kanavaviestin lähetys.
pub const RESPONSE_CHANNEL_MESSAGE: u8 = 4;
/// Discord-vastaus: deferred (agentti vastaa webhookilla myöhemmin).
pub const RESPONSE_DEFERRED_CHANNEL_MESSAGE: u8 = 5;

/// Tarkistaa Discord Interactions -allekirjoituksen (Ed25519).
///
/// `public_key_hex` — 32 tavua hex-muodossa (Developer Portal).
/// `signature_hex` — 64 tavua hex (`X-Signature-Ed25519`).
/// `timestamp` — raakamerkkijono headerista (`X-Signature-Timestamp`).
/// `body` — pyynnön raakabody allekirjoitusta varten.
pub fn verify_signature(
    public_key_hex: &str,
    signature_hex: &str,
    timestamp: &str,
    body: &[u8],
) -> ChannelResult<()> {
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
        .map_err(|_| ChannelError::invalid_input("discord signature verification failed"))
}

/// Parsittu Discord-interaction (MVP: slash-komennot).
#[derive(Debug, Clone)]
pub struct DiscordInteraction {
    /// Discord interaction type (1 = PING, 2 = `APPLICATION_COMMAND`, …).
    pub interaction_type: u8,
    /// Slash-komennon nimi, jos type == 2.
    pub command_name: Option<String>,
    /// `message`-optionin arvo slash-komennossa.
    pub message_text: Option<String>,
    /// Käyttäjän snowflake (member.user.id tai user.id).
    pub user_id: Option<String>,
    /// Kanavan snowflake.
    pub channel_id: Option<String>,
}

impl DiscordInteraction {
    /// Deserialisoi interaction JSON:sta.
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

    /// Muuntaa slash-komennon [`InboundMessage`]:ksi.
    ///
    /// Käyttää `user_id` lähettäjänä ja `channel_id` tai `"discord"` keskusteluna.
    pub fn into_inbound(self) -> ChannelResult<InboundMessage> {
        let body = self
            .message_text
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ChannelError::invalid_input("slash command message option empty"))?;
        let sender = self.user_id.unwrap_or_else(|| "discord-user".to_string());
        let conversation = self.channel_id.unwrap_or_else(|| "discord".to_string());
        InboundMessage::new(sender, conversation, body)
    }

    /// Onko tämä Discord PING (type 1)?
    #[must_use]
    pub fn is_ping(&self) -> bool {
        self.interaction_type == INTERACTION_PING
    }

    /// Onko tämä slash-komento (type 2)?
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
    if s.len() % 2 != 0 {
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

    /// Pieni hex-enkooderi testidataa varten (`decode_hex`:n käänteisoperaatio).
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

        // Deterministinen avain testiä varten (EI tuotantosalaisuus).
        let signing_key = SigningKey::from_bytes(&[0u8; 32]);
        let verifying_key = signing_key.verifying_key();

        let timestamp = "1700000000";
        let body = br#"{"type":1}"#;

        // Discord allekirjoittaa `timestamp || raw_body`.
        let mut message = Vec::new();
        message.extend_from_slice(timestamp.as_bytes());
        message.extend_from_slice(body);
        let signature = signing_key.sign(&message);

        let pk_hex = encode_hex(verifying_key.as_bytes());
        let sig_hex = encode_hex(&signature.to_bytes());

        let result = verify_signature(&pk_hex, &sig_hex, timestamp, body);
        assert!(result.is_ok(), "valid signature must verify: {result:?}");
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

        // Eri body kuin allekirjoitettu → verify epäonnistuu vaikka avain täsmää.
        let tampered_body = br#"{"type":3}"#;
        let err = verify_signature(&pk_hex, &sig_hex, timestamp, tampered_body).unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn verify_rejects_odd_length_hex() {
        // Pariton hex-pituus → decode_hex palauttaa virheen (InvalidInput).
        let err = verify_signature("abc", &"b".repeat(128), "1", b"{}").unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn verify_rejects_wrong_length_public_key() {
        // Validi hex mutta väärä tavumäärä (16 tavua eikä 32) → 32-tavun try_into epäonnistuu.
        let short_pk = "a".repeat(32); // 16 tavua
        let err = verify_signature(&short_pk, &"b".repeat(128), "1", b"{}").unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn verify_rejects_wrong_length_signature() {
        // Validi 32-tavun avain mutta liian lyhyt allekirjoitus (32 tavua eikä 64).
        let pk = "a".repeat(64);
        let short_sig = "b".repeat(64); // 32 tavua
        let err = verify_signature(&pk, &short_sig, "1", b"{}").unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn from_payload_missing_type_errors() {
        // Ei "type"-kenttää → InvalidInput ("interaction missing type").
        let json = serde_json::json!({ "data": { "name": "x" } });
        let err = DiscordInteraction::from_payload(&json).unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn from_payload_type_out_of_range_errors() {
        // type > u8::MAX → u8::try_from epäonnistuu ("interaction type out of range").
        let json = serde_json::json!({ "type": 300 });
        let err = DiscordInteraction::from_payload(&json).unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn from_payload_non_numeric_type_errors() {
        // type ei ole numero → as_u64() palauttaa None → InvalidInput.
        let json = serde_json::json!({ "type": "ping" });
        let err = DiscordInteraction::from_payload(&json).unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn from_payload_without_data_yields_none_fields() {
        // Pelkkä type, ei dataa eikä käyttäjää/kanavaa → kaikki optiot None.
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
        // member.user.id puuttuu → fallback user.id (DM-konteksti).
        let json = serde_json::json!({
            "type": 2,
            "user": { "id": "dm-user-9" }
        });
        let ix = DiscordInteraction::from_payload(&json).expect("parse");
        assert_eq!(ix.user_id.as_deref(), Some("dm-user-9"));
    }

    #[test]
    fn into_inbound_uses_defaults_when_ids_missing() {
        // user_id/channel_id puuttuvat → oletukset "discord-user" ja "discord".
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
        // message-option on pelkkää whitespacea → filter pudottaa → InvalidInput.
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
        // Ei message_text:iä lainkaan → into_inbound antaa InvalidInput.
        let json = serde_json::json!({ "type": 2, "data": { "name": "fc" } });
        let ix = DiscordInteraction::from_payload(&json).expect("parse");
        assert_eq!(ix.message_text, None);
        let err = ix.into_inbound().unwrap_err();
        assert!(matches!(err, ChannelError::InvalidInput(_)));
    }

    #[test]
    fn extract_message_option_ignores_non_message_options() {
        // Vain "channel"-niminen option → message_text jää None:ksi.
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
        // message-option arvo ei ole string (numero) → as_str() None → ei poimita.
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
        // Useita optioita, message poimitaan oikein muiden joukosta.
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
