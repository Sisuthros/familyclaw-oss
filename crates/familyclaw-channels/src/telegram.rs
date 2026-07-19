//! [`TelegramChannel`] — Telegram adapter using long-poll polling of the Bot API.
//!
//! A lightweight Telegram integration that does NOT pull in the heavy
//! `teloxide` SDK:
//! - **Sending** (`sendMessage`) and **receiving** (`getUpdates`) happen
//!   directly against the Telegram Bot API's HTTP REST endpoints via `reqwest`.
//! - Inbound messages are fetched via **long-poll** polling: the
//!   `getUpdates` call blocks on the server for `timeout` seconds until new
//!   updates arrive, then they are acknowledged by advancing `offset` to the
//!   latest `update_id + 1`.
//!
//! ## Why REST instead of teloxide?
//! Same rationale as [`crate::DiscordChannel`] — `teloxide` pulls in dozens
//! of dependencies and its API is still evolving. The Bot API's
//! `getUpdates`/`sendMessage` are stable and sufficient for the MVP. The only
//! extra dependency is `reqwest`, which is already in the workspace (same
//! version as the Discord adapter).
//!
//! ## `getUpdates` offset acknowledgment (Telegram Bot API)
//! `getUpdates` returns an array of updates, each with an increasing
//! `update_id`. The next call supplies `offset = max(update_id) + 1`, which
//! **acknowledges** all updates below that value — the server no longer
//! sends them. This way the same message never arrives twice, and the
//! client doesn't need to deduplicate.
//!
//! ## `conversation` and `channel_id` (invariants #2, #4)
//! Every canonicalized [`InboundEnvelope`] carries:
//! - `channel_id` = the identifier of this channel instance (for routing
//!   replies), and
//! - `conversation` = the Telegram `chat.id` (the same chat the reply is
//!   routed to via `sendMessage`). This way the origin is never lost across
//!   the bus hop.
//!
//! ## Layer A rules
//! The token is never hardcoded: it is read at runtime from the environment
//! (`TELEGRAM_BOT_TOKEN`) or supplied to the constructor. `api_base` is also
//! runtime configuration so tests can point it at a mock server.

use std::sync::{Arc, Mutex};

use tracing::{debug, error, warn};

use crate::channel::{Channel, MessageStream, SendFuture};
use crate::error::{ChannelError, ChannelResult};
use crate::message::{ChannelKind, InboundEnvelope, InboundMessage, OutboundMessage};

/// The environment variable the bot token is read from by default.
const TOKEN_ENV: &str = "TELEGRAM_BOT_TOKEN";

/// Telegram's public Bot API root. The token is appended to the path
/// (`/bot<token>/<method>`).
const DEFAULT_API_BASE: &str = "https://api.telegram.org";

/// Long-poll `getUpdates` timeout in seconds (server-side blocking).
const LONG_POLL_TIMEOUT_SECS: u64 = 30;

/// The HTTP client's overall timeout: long-poll + margin, so the client
/// doesn't cut off the request before the server's own timeout.
const HTTP_TIMEOUT_SECS: u64 = LONG_POLL_TIMEOUT_SECS + 15;

/// Telegram channel using long-poll polling of the Bot API — implements the
/// [`Channel`] interface.
///
/// Receiving starts a background task in the [`Channel::receive`] call: the
/// task polls the `getUpdates` endpoint and pushes each text message into
/// the stream as a canonicalized [`InboundEnvelope`]. Sending (`send`) makes
/// a `sendMessage` request (HTTP `POST`) synchronously.
///
/// All settings (token, `api_base`) are runtime configuration — no
/// hardcoded values.
pub struct TelegramChannel {
    inner: Arc<Inner>,
    /// Receiver for the inbound stream; handed out once in [`Channel::receive`].
    inbound_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<InboundEnvelope>>>,
    /// Sender the background task receives in the [`Channel::receive`] call.
    inbound_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<InboundEnvelope>>>,
}

struct Inner {
    channel_id: String,
    token: String,
    api_base: String,
    client: reqwest::Client,
}

impl TelegramChannel {
    /// Creates a Telegram channel with an explicit token.
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] if the token or channel id is empty,
    /// or if building the HTTP client fails.
    pub fn new(token: impl Into<String>, channel_id: impl Into<String>) -> ChannelResult<Self> {
        Self::with_api_base(token, channel_id, DEFAULT_API_BASE)
    }

    /// Creates a Telegram channel, reading the token from the
    /// `TELEGRAM_BOT_TOKEN` environment variable.
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] if the environment variable is
    /// missing/empty or the channel id is empty.
    pub fn from_env(channel_id: impl Into<String>) -> ChannelResult<Self> {
        let token = std::env::var(TOKEN_ENV).map_err(|_| {
            ChannelError::invalid_input(format!(
                "environment variable {TOKEN_ENV} must be set with the Telegram bot token"
            ))
        })?;
        Self::new(token, channel_id)
    }

    /// Creates a Telegram channel with a custom API root (e.g. a mock server
    /// in tests). The token is appended to the path as `/bot<token>/<method>`.
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] if the token, channel id, or `api_base`
    /// is empty, or if building the HTTP client fails.
    pub fn with_api_base(
        token: impl Into<String>,
        channel_id: impl Into<String>,
        api_base: impl Into<String>,
    ) -> ChannelResult<Self> {
        let token = token.into();
        let channel_id = channel_id.into();
        let api_base = api_base.into();

        if token.trim().is_empty() {
            return Err(ChannelError::invalid_input(
                "Telegram bot token must not be empty",
            ));
        }
        if channel_id.trim().is_empty() {
            return Err(ChannelError::invalid_input("channel_id must not be empty"));
        }
        if api_base.trim().is_empty() {
            return Err(ChannelError::invalid_input("api_base must not be empty"));
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|e| {
                ChannelError::invalid_input(format!("failed to build HTTP client: {e}"))
            })?;

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        Ok(Self {
            inner: Arc::new(Inner {
                channel_id,
                token,
                api_base: api_base.trim_end_matches('/').to_string(),
                client,
            }),
            inbound_rx: Mutex::new(Some(rx)),
            inbound_tx: Mutex::new(Some(tx)),
        })
    }

    /// Builds the method URL: `<api_base>/bot<token>/<method>`.
    fn method_url(inner: &Inner, method: &str) -> String {
        format!("{}/bot{}/{}", inner.api_base, inner.token, method)
    }

    /// One `getUpdates` long-poll round with the given offset. Returns the
    /// parsed inbound messages plus the next offset (`None` if the offset
    /// did not change, i.e. no new updates).
    async fn poll_once(inner: &Inner, offset: Option<i64>) -> ChannelResult<PollOutcome> {
        let mut body = serde_json::json!({
            "timeout": LONG_POLL_TIMEOUT_SECS,
            // Only text messages are of interest — reduces unnecessary traffic.
            "allowed_updates": ["message"],
        });
        if let Some(off) = offset {
            body["offset"] = serde_json::Value::from(off);
        }

        let url = Self::method_url(inner, "getUpdates");
        let response = inner
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| {
                ChannelError::receive(&inner.channel_id, format!("getUpdates HTTP error: {e}"))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ChannelError::receive(
                &inner.channel_id,
                format!("getUpdates returned {status}: {text}"),
            ));
        }

        let text = response.text().await.map_err(|e| {
            ChannelError::receive(&inner.channel_id, format!("getUpdates read body: {e}"))
        })?;

        parse_get_updates(&text, offset)
            .map_err(|reason| ChannelError::receive(&inner.channel_id, reason))
    }

    /// Long-poll loop: polls `getUpdates`, canonicalizes messages, and pushes
    /// them into the stream. Returns once the receiver (`tx`) is closed
    /// (stream dropped) or the error is permanent. On network errors, it
    /// continues after a short delay.
    async fn poll_loop(inner: Arc<Inner>, tx: tokio::sync::mpsc::UnboundedSender<InboundEnvelope>) {
        let mut offset: Option<i64> = None;
        loop {
            if tx.is_closed() {
                debug!(channel = %inner.channel_id, "Telegram poll loop: stream closed, stopping");
                return;
            }

            match Self::poll_once(&inner, offset).await {
                Ok(outcome) => {
                    if let Some(next) = outcome.next_offset {
                        offset = Some(next);
                    }
                    for inbound in outcome.messages {
                        let env =
                            inbound.into_envelope(ChannelKind::Telegram, inner.channel_id.clone());
                        if tx.send(env).is_err() {
                            debug!(
                                channel = %inner.channel_id,
                                "Telegram poll loop: receiver dropped, stopping"
                            );
                            return;
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        channel = %inner.channel_id,
                        error = %e,
                        "Telegram getUpdates failed; retrying after backoff"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
            }
        }
    }

    async fn send_message(inner: &Inner, message: &OutboundMessage) -> ChannelResult<()> {
        match message.kind {
            crate::message::OutboundKind::Typing => {
                let payload = serde_json::json!({
                    "chat_id": message.target,
                    "action": "typing",
                });
                let url = Self::method_url(inner, "sendChatAction");
                let response = inner
                    .client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .body(payload.to_string())
                    .send()
                    .await
                    .map_err(|e| {
                        ChannelError::send(
                            &inner.channel_id,
                            format!("sendChatAction HTTP error: {e}"),
                        )
                    })?;
                if !response.status().is_success() {
                    let status = response.status();
                    let body = response.status().to_string();
                    return Err(ChannelError::send(
                        &inner.channel_id,
                        format!("sendChatAction returned {status}: {body}"),
                    ));
                }
                debug!(channel = %inner.channel_id, "Telegram typing indicator sent");
                Ok(())
            }
            crate::message::OutboundKind::Message | crate::message::OutboundKind::Progress => {
                let payload = serde_json::json!({
                    "chat_id": message.target,
                    "text": message.body,
                });

                let url = Self::method_url(inner, "sendMessage");
                let response = inner
                    .client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .body(payload.to_string())
                    .send()
                    .await
                    .map_err(|e| {
                        ChannelError::send(
                            &inner.channel_id,
                            format!("sendMessage HTTP error: {e}"),
                        )
                    })?;

                if !response.status().is_success() {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    error!(
                        channel = %inner.channel_id,
                        %status,
                        %body,
                        "Telegram sendMessage returned error"
                    );
                    return Err(ChannelError::send(
                        &inner.channel_id,
                        format!("sendMessage returned {status}: {body}"),
                    ));
                }

                debug!(channel = %inner.channel_id, "Telegram sendMessage sent successfully");
                Ok(())
            }
        }
    }
}

impl std::fmt::Debug for TelegramChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The token never ends up in logs/Debug output (Layer A: no secrets).
        f.debug_struct("TelegramChannel")
            .field("channel_id", &self.inner.channel_id)
            .field("api_base", &self.inner.api_base)
            .finish_non_exhaustive()
    }
}

impl Channel for TelegramChannel {
    fn channel_id(&self) -> &str {
        &self.inner.channel_id
    }

    fn kind(&self) -> ChannelKind {
        ChannelKind::Telegram
    }

    fn send(&self, message: OutboundMessage) -> SendFuture<'_> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            Self::send_message(&inner, &message).await?;
            Ok(())
        })
    }

    fn receive(&self) -> ChannelResult<MessageStream> {
        let rx = self
            .inbound_rx
            .lock()
            .map_err(|_| ChannelError::backend(self.channel_id(), "inbound_rx lock poisoned"))?
            .take()
            .ok_or_else(|| {
                ChannelError::receive(self.channel_id(), "receive stream already taken")
            })?;

        // Hand over the sender to the background task (once). Start the long-poll.
        let tx = self
            .inbound_tx
            .lock()
            .map_err(|_| ChannelError::backend(self.channel_id(), "inbound_tx lock poisoned"))?
            .take()
            .ok_or_else(|| {
                ChannelError::receive(self.channel_id(), "inbound sender already taken")
            })?;

        let inner = Arc::clone(&self.inner);
        tokio::spawn(Self::poll_loop(inner, tx));

        Ok(MessageStream::new(rx))
    }
}

/// The result of a `getUpdates` round: parsed messages + the next offset.
#[derive(Debug, Default, PartialEq, Eq)]
struct PollOutcome {
    /// Text messages received in this round (ready for canonicalization).
    messages: Vec<InboundMessage>,
    /// The offset for the next `getUpdates` call (`max(update_id) + 1`).
    /// `None` if no updates arrived, in which case the previous offset is kept.
    next_offset: Option<i64>,
}

/// Parses a `getUpdates` JSON response into messages + the next offset.
///
/// This is a **pure function** (no network) so long-poll parsing is
/// unit-testable without a real Telegram server. Logic:
/// - `ok: false` → an error (based on `description`).
/// - each `result[]` update whose `message.text` is non-empty → one
///   [`InboundMessage`] (`sender` = `from.id`, `conversation` = `chat.id`).
/// - offset acknowledgment: `next_offset = max(update_id) + 1` across all
///   seen updates (including non-text updates, so they don't arrive again).
///   `prev_offset` is kept if no updates arrived.
///
/// # Errors
/// A string error if the JSON is malformed or `ok` is not `true`.
fn parse_get_updates(body: &str, prev_offset: Option<i64>) -> Result<PollOutcome, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid getUpdates JSON: {e}"))?;

    let ok = value.get("ok").and_then(serde_json::Value::as_bool);
    if ok != Some(true) {
        let desc = value
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown Telegram API error");
        let code = value
            .get("error_code")
            .and_then(serde_json::Value::as_i64)
            .map_or_else(String::new, |c| format!(" (error_code {c})"));
        return Err(format!("Telegram getUpdates ok=false: {desc}{code}"));
    }

    let results = value
        .get("result")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "getUpdates response missing 'result' array".to_string())?;

    let mut messages = Vec::new();
    let mut max_update_id: Option<i64> = None;

    for update in results {
        // Acknowledge every update seen (including non-text), so it doesn't repeat.
        if let Some(uid) = update.get("update_id").and_then(serde_json::Value::as_i64) {
            max_update_id = Some(max_update_id.map_or(uid, |m: i64| m.max(uid)));
        }

        let Some(msg) = update.get("message") else {
            continue;
        };
        let Some(text) = msg.get("text").and_then(serde_json::Value::as_str) else {
            // A non-text message (photo/sticker/…): skipped for content, but
            // the update_id is already acknowledged above.
            continue;
        };
        if text.is_empty() {
            continue;
        }

        // conversation = chat.id (invariant #4: the reply address is preserved).
        let Some(chat_id) = msg
            .get("chat")
            .and_then(|c| c.get("id"))
            .and_then(serde_json::Value::as_i64)
        else {
            continue;
        };

        // sender = from.id; falls back to chat.id if 'from' is missing (e.g.
        // channel posts). An empty sender is not allowed (InboundMessage::new).
        let sender_id = msg
            .get("from")
            .and_then(|fr| fr.get("id"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(chat_id);

        // A single invalid message does not abort the whole round — it is skipped.
        if let Ok(inbound) = InboundMessage::new(sender_id.to_string(), chat_id.to_string(), text) {
            messages.push(inbound);
        }
    }

    let next_offset = max_update_id.map_or(prev_offset, |m| Some(m + 1));

    Ok(PollOutcome {
        messages,
        next_offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_empty_token() {
        assert!(TelegramChannel::new("", "tg-1").is_err());
        assert!(TelegramChannel::new("   ", "tg-1").is_err());
    }

    #[test]
    fn new_rejects_empty_channel_id() {
        assert!(TelegramChannel::new("token", "  ").is_err());
    }

    #[test]
    fn new_ok_with_token_and_id() {
        let ch = TelegramChannel::new("123:ABC", "tg-main").expect("channel");
        assert_eq!(ch.channel_id(), "tg-main");
        assert_eq!(ch.kind(), ChannelKind::Telegram);
    }

    #[test]
    fn debug_does_not_leak_token() {
        let ch = TelegramChannel::new("SECRET-TOKEN-123", "tg-1").expect("channel");
        let dbg = format!("{ch:?}");
        assert!(dbg.contains("TelegramChannel"));
        assert!(dbg.contains("tg-1"));
        assert!(
            !dbg.contains("SECRET-TOKEN-123"),
            "token must not appear in Debug output"
        );
    }

    #[test]
    fn method_url_builds_bot_path() {
        let ch = TelegramChannel::with_api_base("TKN", "tg-1", "https://example.test/")
            .expect("channel");
        // The trailing slash is already trimmed; the path is /bot<token>/<method>.
        let url = TelegramChannel::method_url(&ch.inner, "getUpdates");
        assert_eq!(url, "https://example.test/botTKN/getUpdates");
    }

    #[test]
    fn from_env_errors_when_unset() {
        // Make sure the variable is not set in this test.
        std::env::remove_var(TOKEN_ENV);
        assert!(TelegramChannel::from_env("tg-1").is_err());
    }

    // --- parse_get_updates: long-poll parsing (no network) ---

    #[test]
    fn parse_single_text_message() {
        let body = r#"{
            "ok": true,
            "result": [
                {
                    "update_id": 100,
                    "message": {
                        "message_id": 7,
                        "from": { "id": 4242, "first_name": "User" },
                        "chat": { "id": -1009, "type": "group" },
                        "text": "moi"
                    }
                }
            ]
        }"#;
        let outcome = parse_get_updates(body, None).expect("parse ok");
        assert_eq!(outcome.messages.len(), 1);
        let m = &outcome.messages[0];
        assert_eq!(m.sender, "4242");
        // conversation = chat.id (invariant #4)
        assert_eq!(m.conversation, "-1009");
        assert_eq!(m.body, "moi");
        // offset acknowledgment: max(update_id) + 1
        assert_eq!(outcome.next_offset, Some(101));
    }

    #[test]
    fn parse_multiple_updates_advances_offset_to_max_plus_one() {
        let body = r#"{
            "ok": true,
            "result": [
                { "update_id": 5, "message": { "chat": {"id": 1}, "from": {"id": 1}, "text": "a" } },
                { "update_id": 7, "message": { "chat": {"id": 1}, "from": {"id": 1}, "text": "b" } },
                { "update_id": 6, "message": { "chat": {"id": 1}, "from": {"id": 1}, "text": "c" } }
            ]
        }"#;
        let outcome = parse_get_updates(body, Some(5)).expect("parse ok");
        assert_eq!(outcome.messages.len(), 3);
        // max update_id = 7 → next offset 8 (acknowledges 5,6,7).
        assert_eq!(outcome.next_offset, Some(8));
    }

    #[test]
    fn parse_empty_result_keeps_previous_offset() {
        let body = r#"{ "ok": true, "result": [] }"#;
        let outcome = parse_get_updates(body, Some(42)).expect("parse ok");
        assert!(outcome.messages.is_empty());
        // No new updates → the previous offset is kept.
        assert_eq!(outcome.next_offset, Some(42));
    }

    #[test]
    fn parse_non_text_update_is_acked_but_not_emitted() {
        // A sticker/photo update without a 'text' field: no message, but the
        // update_id is acknowledged so it doesn't arrive again.
        let body = r#"{
            "ok": true,
            "result": [
                { "update_id": 200, "message": { "chat": {"id": 9}, "from": {"id": 9}, "sticker": {} } }
            ]
        }"#;
        let outcome = parse_get_updates(body, None).expect("parse ok");
        assert!(outcome.messages.is_empty());
        assert_eq!(outcome.next_offset, Some(201));
    }

    #[test]
    fn parse_message_without_from_falls_back_to_chat_id_as_sender() {
        // In a channel post 'from' may be missing → sender = chat.id.
        let body = r#"{
            "ok": true,
            "result": [
                { "update_id": 1, "message": { "chat": {"id": 555}, "text": "channel post" } }
            ]
        }"#;
        let outcome = parse_get_updates(body, None).expect("parse ok");
        assert_eq!(outcome.messages.len(), 1);
        assert_eq!(outcome.messages[0].sender, "555");
        assert_eq!(outcome.messages[0].conversation, "555");
    }

    #[test]
    fn parse_skips_empty_text() {
        let body = r#"{
            "ok": true,
            "result": [
                { "update_id": 3, "message": { "chat": {"id": 1}, "from": {"id": 1}, "text": "" } }
            ]
        }"#;
        let outcome = parse_get_updates(body, None).expect("parse ok");
        assert!(outcome.messages.is_empty());
        // update_id is still acknowledged (so an empty message doesn't loop forever).
        assert_eq!(outcome.next_offset, Some(4));
    }

    #[test]
    fn parse_ok_false_is_error() {
        let body = r#"{ "ok": false, "error_code": 401, "description": "Unauthorized" }"#;
        let err = parse_get_updates(body, None).expect_err("ok=false must error");
        assert!(err.contains("Unauthorized"));
        assert!(err.contains("401"));
    }

    #[test]
    fn parse_invalid_json_is_error() {
        assert!(parse_get_updates("not json", None).is_err());
    }

    #[test]
    fn parse_missing_result_array_is_error() {
        let body = r#"{ "ok": true }"#;
        assert!(parse_get_updates(body, None).is_err());
    }

    #[test]
    fn parse_canonicalizes_into_telegram_envelope() {
        // Round-trip: the parsed InboundMessage → InboundEnvelope preserves
        // channel_id + conversation (invariants #2 and #4).
        let body = r#"{
            "ok": true,
            "result": [
                { "update_id": 10, "message": { "chat": {"id": 77}, "from": {"id": 88}, "text": "hi" } }
            ]
        }"#;
        let outcome = parse_get_updates(body, None).expect("parse ok");
        let env = outcome.messages[0]
            .clone()
            .into_envelope(ChannelKind::Telegram, "tg-main");
        assert_eq!(env.kind, ChannelKind::Telegram);
        assert_eq!(env.channel_id, "tg-main"); // #2
        assert_eq!(env.conversation, "77"); // #4
        assert_eq!(env.sender, "88");
        assert_eq!(env.body, "hi");
        // The reply is routed back to the same chat.
        let reply = env.reply("pong").expect("reply");
        assert_eq!(reply.target, "77");
    }
}
