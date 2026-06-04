//! [`DiscordChannel`] — Discord-adapteri HTTP-webhookin kautta.
//!
//! Kevyt Discord-integraatio joka ei vaadi raskasta serenity-gatewayta.
//! Lähetys tapahtuu Discord-webhookin kautta (HTTP POST), vastaanotto
//! jätetään myöhemmälle (gateway-integraatio KERROS B:ssä).
//!
//! ## Miksi webhook eikä serenity?
//!
//! Serenity vetää sisään kymmeniä riippuvuuksia ja sen API muuttuu
//! jatkuvasti. Webhook-lähestymistapa on:
//! - Kevyempi (vain reqwest)
//! - Vakaampi (REST API ei muutu yhtä usein)
//! - Riittävä MVP:hen (lähettää viestejä Discordiin)
//! - Vastaanotto voidaan lisätä myöhemmin gateway-integraatiolla
//!
//! ## KERROS A -säännöt
//! Kaikki konfiguraatio (webhook_url) on ajonaikaista — ei kovakoodattuja
//! arvoja.

use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{debug, error};

use crate::channel::{Channel, MessageStream, SendFuture};
use crate::error::{ChannelError, ChannelResult};
use crate::message::{ChannelKind, InboundEnvelope, OutboundMessage};

/// Discord-kanava webhook-lähetys — toteuttaa [`Channel`]-rajapinnan.
///
/// Lähettää viestejä Discord-webhookin kautta HTTP POSTilla.
/// Vastaanotto (gateway polling) lisätään myöhemmässä vaiheessa;
/// tällä hetkellä `receive()` palauttaa tyhjän virran ja injektointi
/// tapahtuu manuaalisesti `inject()`:llä (sama malli kuin MockChannel).
///
/// Kaikki asetukset ovat ajonaikaisia — ei kovakoodattuja arvoja.
pub struct DiscordChannel {
    inner: Arc<Inner>,
}

struct Inner {
    channel_id: String,
    webhook_url: String,
    client: reqwest::Client,
    outbox: Mutex<Vec<OutboundMessage>>,
    inbound_tx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<InboundEnvelope>>>,
    inbound_rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<InboundEnvelope>>>,
}

impl DiscordChannel {
    /// Luo uuden Discord-kanavan webhook-lähetyksellä.
    ///
    /// # Errors
    /// Palauttaa virheen jos webhook_url on tyhjä.
    pub fn new(
        webhook_url: impl Into<String>,
        channel_name: impl Into<String>,
    ) -> ChannelResult<Self> {
        let webhook_url = webhook_url.into();
        let channel_id = channel_name.into();

        if webhook_url.trim().is_empty() {
            return Err(ChannelError::invalid_input("webhook URL must not be empty"));
        }

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        Ok(Self {
            inner: Arc::new(Inner {
                channel_id,
                webhook_url,
                client: reqwest::Client::new(),
                outbox: Mutex::new(Vec::new()),
                inbound_tx: std::sync::Mutex::new(Some(tx)),
                inbound_rx: std::sync::Mutex::new(Some(rx)),
            }),
        })
    }

    /// Luo kanavan kustomoidulla tyypillä (esim. `ChannelKind::Discord`).
    ///
    /// Käytetään kun kanavan tunniste halutaan antaa erikseen.
    pub fn with_kind(
        webhook_url: impl Into<String>,
        channel_id: impl Into<String>,
    ) -> ChannelResult<Self> {
        Self::new(webhook_url, channel_id)
    }

    /// Injektoi saapuvan viestin kanavavirtaan.
    ///
    /// Sama malli kuin MockChannel — kun oikea gateway-integraatio
    /// lisätään (KERROS B), tämä korvataan automaattisella pollauksella.
    pub fn inject(&self, envelope: InboundEnvelope) -> ChannelResult<()> {
        let tx = self.inner.inbound_tx.lock().unwrap();
        match tx.as_ref() {
            Some(sender) => sender
                .send(envelope)
                .map_err(|e| ChannelError::receive(&self.inner.channel_id, e.to_string())),
            None => Err(ChannelError::closed(&self.inner.channel_id)),
        }
    }

    /// Palauttaa lähetetyt viestit (testaustarkoitus).
    pub async fn sent(&self) -> Vec<OutboundMessage> {
        self.inner.outbox.lock().await.clone()
    }

    /// Sulkee kanavan — ei enää vastaanottoa.
    pub fn close_inbound(&self) {
        if let Ok(mut tx) = self.inner.inbound_tx.lock() {
            *tx = None;
        }
    }

    async fn send_to_webhook(inner: &Inner, message: &OutboundMessage) -> ChannelResult<()> {
        let username = message.target.as_str();
        let payload = serde_json::json!({
            "content": message.body,
            "username": username,
        });

        let response = inner
            .client
            .post(&inner.webhook_url)
            .header("Content-Type", "application/json")
            .body(payload.to_string())
            .send()
            .await
            .map_err(|e| {
                ChannelError::send(&inner.channel_id, format!("webhook HTTP error: {e}"))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            error!(
                channel = %inner.channel_id,
                %status,
                %body,
                "Discord webhook returned error"
            );
            return Err(ChannelError::send(
                &inner.channel_id,
                format!("webhook returned {status}: {body}"),
            ));
        }

        debug!(channel = %inner.channel_id, "Discord webhook sent successfully");
        Ok(())
    }
}

impl Channel for DiscordChannel {
    fn channel_id(&self) -> &str {
        &self.inner.channel_id
    }

    fn kind(&self) -> ChannelKind {
        ChannelKind::Discord
    }

    fn send(&self, message: OutboundMessage) -> SendFuture<'_> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            Self::send_to_webhook(&inner, &message).await?;
            inner.outbox.lock().await.push(message);
            Ok(())
        })
    }

    fn receive(&self) -> ChannelResult<MessageStream> {
        let rx = self
            .inner
            .inbound_rx
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| {
                ChannelError::receive(&self.inner.channel_id, "receive stream already taken")
            })?;
        Ok(MessageStream::new(rx))
    }
}
