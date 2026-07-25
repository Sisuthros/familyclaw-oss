//! [`SlackChannel`] — Slack Web API adapter (HTTP, no heavy SDK).
//!
//! MVP capabilities:
//! - **Outbound:** `chat.postMessage` via `SLACK_BOT_TOKEN`
//! - **Inbound:** **not wired.** [`SlackChannel::inject`] exists, but no HTTP
//!   route reaches it — the gateway's `POST /inject` builds a Discord envelope
//!   and requires a configured Discord channel, and Socket Mode / Events API is
//!   not implemented. Inbound Slack messages do not reach the agent today.
//! - **Approvals:** [`format_approval_prompt`] renders Approve/Deny instructions
//!   with the gateway approval id for one-click follow-up in `/console`
//!
//! Credentials are runtime-only (Layer A).

use std::sync::{Arc, Mutex};

use tracing::{debug, error};

use crate::channel::{Channel, MessageStream, SendFuture};
use crate::error::{ChannelError, ChannelResult};
use crate::message::{ChannelKind, InboundEnvelope, InboundMessage, OutboundKind, OutboundMessage};

/// Bot token environment variable.
pub const TOKEN_ENV: &str = "SLACK_BOT_TOKEN";

/// Default Slack Web API root.
const DEFAULT_API_BASE: &str = "https://slack.com/api";

/// Slack channel using the Web API — implements [`Channel`].
pub struct SlackChannel {
    inner: Arc<Inner>,
    inbound_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<InboundEnvelope>>>,
    inbound_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<InboundEnvelope>>>,
}

struct Inner {
    channel_id: String,
    token: String,
    api_base: String,
    client: reqwest::Client,
}

impl SlackChannel {
    /// Creates a Slack channel with an explicit bot token.
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] if token or channel id is empty.
    pub fn new(token: impl Into<String>, channel_id: impl Into<String>) -> ChannelResult<Self> {
        Self::with_api_base(token, channel_id, DEFAULT_API_BASE)
    }

    /// Creates a Slack channel from `SLACK_BOT_TOKEN`.
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] if the env var is missing/empty.
    pub fn from_env(channel_id: impl Into<String>) -> ChannelResult<Self> {
        let token = std::env::var(TOKEN_ENV).map_err(|_| {
            ChannelError::invalid_input(format!("{TOKEN_ENV} must be set for Slack channel"))
        })?;
        Self::new(token, channel_id)
    }

    /// Test seam: custom API base (mock server).
    pub fn with_api_base(
        token: impl Into<String>,
        channel_id: impl Into<String>,
        api_base: impl Into<String>,
    ) -> ChannelResult<Self> {
        let token = token.into();
        let channel_id = channel_id.into();
        let api_base = api_base.into().trim_end_matches('/').to_string();
        if token.trim().is_empty() {
            return Err(ChannelError::invalid_input(
                "Slack bot token must not be empty",
            ));
        }
        if channel_id.trim().is_empty() {
            return Err(ChannelError::invalid_input(
                "Slack channel_id must not be empty",
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ChannelError::invalid_input(format!("HTTP client: {e}")))?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Ok(Self {
            inner: Arc::new(Inner {
                channel_id,
                token,
                api_base,
                client,
            }),
            inbound_rx: Mutex::new(Some(rx)),
            inbound_tx: Mutex::new(Some(tx)),
        })
    }

    /// Injects an inbound message (Events API / gateway inject / tests).
    ///
    /// # Errors
    /// [`ChannelError::Receive`] if the inbound stream was already taken/closed.
    pub fn inject(&self, message: InboundMessage) -> ChannelResult<()> {
        let tx = self
            .inbound_tx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(tx) = tx.as_ref() else {
            return Err(ChannelError::receive(
                &self.inner.channel_id,
                "inbound stream not available",
            ));
        };
        let env = message.into_envelope(ChannelKind::Slack, self.inner.channel_id.clone());
        tx.send(env).map_err(|_| {
            ChannelError::receive(&self.inner.channel_id, "inbound receiver dropped")
        })?;
        Ok(())
    }

    /// Formats an operator-facing approval prompt for Slack (and console).
    #[must_use]
    pub fn format_approval_prompt(approval_id: &str, summary: &str, gateway_base: &str) -> String {
        let base = gateway_base.trim_end_matches('/');
        format!(
            "*Approval needed*\n{summary}\n\n\
             Approve: `POST {base}/approvals/{approval_id}/approve`\n\
             Deny: `POST {base}/approvals/{approval_id}/deny`\n\
             Or open the Reliability Console: `{base}/console`"
        )
    }

    fn method_url(inner: &Inner, method: &str) -> String {
        format!("{}/{}", inner.api_base, method)
    }

    async fn send_message(inner: &Inner, message: &OutboundMessage) -> ChannelResult<()> {
        match message.kind {
            OutboundKind::Typing => {
                // Slack has no direct typing API in the bot token subset we use;
                // treat as a no-op success so the tool loop stays calm.
                debug!(channel = %inner.channel_id, "Slack typing indicator skipped (no-op)");
                Ok(())
            }
            OutboundKind::Message | OutboundKind::Progress => {
                let payload = serde_json::json!({
                    "channel": message.target,
                    "text": message.body,
                });
                let url = Self::method_url(inner, "chat.postMessage");
                let response = inner
                    .client
                    .post(&url)
                    .bearer_auth(&inner.token)
                    .header("Content-Type", "application/json")
                    .body(payload.to_string())
                    .send()
                    .await
                    .map_err(|e| {
                        ChannelError::send(&inner.channel_id, format!("chat.postMessage HTTP: {e}"))
                    })?;
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                if !status.is_success() {
                    error!(channel = %inner.channel_id, %status, %body, "Slack chat.postMessage failed");
                    return Err(ChannelError::send(
                        &inner.channel_id,
                        format!("chat.postMessage returned {status}: {body}"),
                    ));
                }
                // Slack returns 200 with ok:false for many auth errors.
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                    if v.get("ok") == Some(&serde_json::Value::Bool(false)) {
                        let err = v.get("error").and_then(|e| e.as_str()).unwrap_or("unknown");
                        return Err(ChannelError::send(
                            &inner.channel_id,
                            format!("chat.postMessage ok=false: {err}"),
                        ));
                    }
                }
                debug!(channel = %inner.channel_id, "Slack chat.postMessage ok");
                Ok(())
            }
        }
    }
}

impl std::fmt::Debug for SlackChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackChannel")
            .field("channel_id", &self.inner.channel_id)
            .field("api_base", &self.inner.api_base)
            .finish_non_exhaustive()
    }
}

impl Channel for SlackChannel {
    fn channel_id(&self) -> &str {
        &self.inner.channel_id
    }

    fn kind(&self) -> ChannelKind {
        ChannelKind::Slack
    }

    fn send(&self, message: OutboundMessage) -> SendFuture<'_> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            Self::send_message(&inner, &message).await?;
            Ok(())
        })
    }

    fn receive(&self) -> ChannelResult<MessageStream> {
        let mut guard = self
            .inbound_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let rx = guard.take().ok_or_else(|| {
            ChannelError::receive(&self.inner.channel_id, "receive() already called")
        })?;
        Ok(MessageStream::new(rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_token() {
        let err = SlackChannel::new("", "C123").expect_err("empty token");
        assert!(err.to_string().contains("token"));
    }

    #[test]
    fn approval_prompt_contains_console_and_ids() {
        let text =
            SlackChannel::format_approval_prompt("appr-1", "Refund €25", "http://127.0.0.1:8787");
        assert!(text.contains("appr-1"));
        assert!(text.contains("/console"));
        assert!(text.contains("Refund"));
    }

    #[tokio::test]
    async fn inject_delivers_envelope() {
        let ch = SlackChannel::with_api_base("xoxb-test", "slack-main", "http://127.0.0.1:9")
            .expect("channel");
        let mut stream = ch.receive().expect("stream");
        ch.inject(InboundMessage::new("U1", "C1", "hello").expect("msg"))
            .expect("inject");
        let env = stream.recv().await.expect("one message");
        assert_eq!(env.kind, ChannelKind::Slack);
        assert_eq!(env.body, "hello");
        assert_eq!(env.channel_id, "slack-main");
    }
}
