//! [`TelegramChannel`] — Telegram-adapteri Bot API:n long-poll-pollauksella.
//!
//! Kevyt Telegram-integraatio joka EI vedä sisään raskasta `teloxide`-SDK:ta:
//! - **Lähetys** (`sendMessage`) ja **vastaanotto** (`getUpdates`) tapahtuvat
//!   suoraan Telegram Bot API:n HTTP-REST-endpointeilla `reqwest`:illä.
//! - Saapuvat viestit haetaan **long-poll**-pollauksella: `getUpdates`-kutsu
//!   blokkaa palvelimella `timeout`-sekuntia kunnes uusia päivityksiä tulee,
//!   sitten kuitataan ne nostamalla `offset` viimeisimmän `update_id + 1`:een.
//!
//! ## Miksi REST eikä teloxide?
//! Sama linja kuin [`crate::DiscordChannel`]:llä — `teloxide` vetää kymmeniä
//! riippuvuuksia ja sen API elää. Bot API:n `getUpdates`/`sendMessage` ovat
//! vakaita ja riittävät MVP:hen. Ainoa lisäriippuvuus on `reqwest`, joka on jo
//! workspacessa (sama versio kuin Discord-adapterilla).
//!
//! ## `getUpdates`-offset-kuittaus (Telegram Bot API)
//! `getUpdates` palauttaa taulukon päivityksiä, kukin nousevalla `update_id`:llä.
//! Seuraavassa kutsussa annetaan `offset = max(update_id) + 1`, mikä **kuittaa**
//! kaikki sitä pienemmät päivitykset — palvelin ei lähetä niitä enää. Näin
//! sama viesti ei tule kahdesti, eikä klientin tarvitse deduplikoida.
//!
//! ## `conversation` ja `channel_id` (invariantit #2, #4)
//! Jokainen kanonisoitu [`InboundEnvelope`] kantaa:
//! - `channel_id` = tämän kanavainstanssin tunniste (vastaajan reititystä
//!   varten), ja
//! - `conversation` = Telegram `chat.id` (sama chat johon vastaus ohjataan
//!   `sendMessage`:lla). Näin alkuperä ei katoa bus-hopissa.
//!
//! ## KERROS A -säännöt
//! Tokenia ei kovakoodata: se luetaan ajonaikaisesti ympäristöstä
//! (`TELEGRAM_BOT_TOKEN`) tai annetaan konstruktorille. `api_base` on myös
//! ajonaikaista, jotta testit voivat osoittaa mock-palvelimelle.

use std::sync::{Arc, Mutex};

use tracing::{debug, error, warn};

use crate::channel::{Channel, MessageStream, SendFuture};
use crate::error::{ChannelError, ChannelResult};
use crate::message::{ChannelKind, InboundEnvelope, InboundMessage, OutboundMessage};

/// Ympäristömuuttuja josta bot-token luetaan oletuksena.
const TOKEN_ENV: &str = "TELEGRAM_BOT_TOKEN";

/// Telegramin julkinen Bot API -juuri. Token liitetään polkuun
/// (`/bot<token>/<method>`).
const DEFAULT_API_BASE: &str = "https://api.telegram.org";

/// Long-poll `getUpdates`-timeout sekunteina (palvelinpuolen blokkaus).
const LONG_POLL_TIMEOUT_SECS: u64 = 30;

/// HTTP-asiakkaan kokonaistimeout: long-poll + marginaali, jotta klientti ei
/// katkaise pyyntöä ennen palvelimen omaa timeoutia.
const HTTP_TIMEOUT_SECS: u64 = LONG_POLL_TIMEOUT_SECS + 15;

/// Telegram-kanava Bot API:n long-poll-pollauksella — toteuttaa
/// [`Channel`]-rajapinnan.
///
/// Vastaanotto käynnistää taustatehtävän [`Channel::receive`]-kutsussa: tehtävä
/// pollaa `getUpdates`-endpointtia ja työntää jokaisen tekstiviestin
/// kanonisoituna [`InboundEnvelope`]:nä virtaan. Lähetys (`send`) tekee
/// `sendMessage`-pyynnön (HTTP `POST`) synkronisesti.
///
/// Kaikki asetukset (token, `api_base`) ovat ajonaikaisia — ei kovakoodattuja
/// arvoja.
pub struct TelegramChannel {
    inner: Arc<Inner>,
    /// Saapuvan virran vastaanotin; luovutetaan kerran [`Channel::receive`]:ssä.
    inbound_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<InboundEnvelope>>>,
    /// Lähetin jonka taustatehtävä saa [`Channel::receive`]-kutsussa.
    inbound_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<InboundEnvelope>>>,
}

struct Inner {
    channel_id: String,
    token: String,
    api_base: String,
    client: reqwest::Client,
}

impl TelegramChannel {
    /// Luo Telegram-kanavan eksplisiittisellä tokenilla.
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] jos token tai kanavatunniste on tyhjä,
    /// tai jos HTTP-asiakkaan rakennus epäonnistuu.
    pub fn new(token: impl Into<String>, channel_id: impl Into<String>) -> ChannelResult<Self> {
        Self::with_api_base(token, channel_id, DEFAULT_API_BASE)
    }

    /// Luo Telegram-kanavan lukien tokenin `TELEGRAM_BOT_TOKEN`-ympäristö-
    /// muuttujasta.
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] jos ympäristömuuttuja puuttuu/on tyhjä
    /// tai kanavatunniste on tyhjä.
    pub fn from_env(channel_id: impl Into<String>) -> ChannelResult<Self> {
        let token = std::env::var(TOKEN_ENV).map_err(|_| {
            ChannelError::invalid_input(format!(
                "environment variable {TOKEN_ENV} must be set with the Telegram bot token"
            ))
        })?;
        Self::new(token, channel_id)
    }

    /// Luo Telegram-kanavan kustomoidulla API-juurella (esim. mock-palvelin
    /// testeissä). Token liitetään polkuun muodossa `/bot<token>/<method>`.
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] jos token, kanavatunniste tai `api_base`
    /// on tyhjä, tai jos HTTP-asiakkaan rakennus epäonnistuu.
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

    /// Rakentaa metodi-URL:n: `<api_base>/bot<token>/<method>`.
    fn method_url(inner: &Inner, method: &str) -> String {
        format!("{}/bot{}/{}", inner.api_base, inner.token, method)
    }

    /// Yksi `getUpdates`-long-poll-kierros annetulla offsetilla. Palauttaa
    /// jäsennetyt saapuvat viestit sekä seuraavan offsetin (`None` jos offset ei
    /// muuttunut, ts. ei uusia päivityksiä).
    async fn poll_once(inner: &Inner, offset: Option<i64>) -> ChannelResult<PollOutcome> {
        let mut body = serde_json::json!({
            "timeout": LONG_POLL_TIMEOUT_SECS,
            // Vain tekstiviestit kiinnostavat — vähentää turhaa liikennettä.
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

    /// Long-poll-silmukka: pollaa `getUpdates`, kanonisoi viestit ja työntää ne
    /// virtaan. Palaa kun vastaanotin (`tx`) on suljettu (stream pudotettu) tai
    /// virhe on pysyvä. Verkkovirheissä jatkaa pienen viiveen jälkeen.
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
                ChannelError::send(&inner.channel_id, format!("sendMessage HTTP error: {e}"))
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

impl std::fmt::Debug for TelegramChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Token EI päädy lokeihin/Debug-tulosteeseen (KERROS A: ei salaisuuksia).
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

        // Luovuta lähetin taustatehtävälle (kerran). Käynnistä long-poll.
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

/// `getUpdates`-kierroksen tulos: jäsennetyt viestit + seuraava offset.
#[derive(Debug, Default, PartialEq, Eq)]
struct PollOutcome {
    /// Tällä kierroksella saapuneet tekstiviestit (kanonisoitavissa).
    messages: Vec<InboundMessage>,
    /// Seuraavan `getUpdates`-kutsun offset (`max(update_id) + 1`). `None` jos
    /// päivityksiä ei tullut, jolloin edellinen offset säilytetään.
    next_offset: Option<i64>,
}

/// Jäsentää `getUpdates`-JSON-vastauksen viesteiksi + seuraavaksi offsetiksi.
///
/// Tämä on **puhdas funktio** (ei verkkoa), jotta long-poll-parsinta on
/// yksikkötestattavissa ilman oikeaa Telegram-palvelinta. Logiikka:
/// - `ok: false` → virhe (`description` mukaan).
/// - jokainen `result[]`-päivitys jonka `message.text` on ei-tyhjä → yksi
///   [`InboundMessage`] (`sender` = `from.id`, `conversation` = `chat.id`).
/// - offset-kuittaus: `next_offset = max(update_id) + 1` kaikista nähdyistä
///   päivityksistä (myös ei-teksti-päivityksistä, jotta ne eivät tule
///   uudestaan). `prev_offset` säilyy jos päivityksiä ei tullut.
///
/// # Errors
/// Merkkijono-virheen jos JSON on viallinen tai `ok` ei ole `true`.
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
        // Kuittaa jokainen nähty päivitys (myös ei-teksti), jotta se ei toistu.
        if let Some(uid) = update.get("update_id").and_then(serde_json::Value::as_i64) {
            max_update_id = Some(max_update_id.map_or(uid, |m: i64| m.max(uid)));
        }

        let Some(msg) = update.get("message") else {
            continue;
        };
        let Some(text) = msg.get("text").and_then(serde_json::Value::as_str) else {
            // Ei-teksti-viesti (kuva/sticker/…): ohitetaan sisällöltä, mutta
            // update_id on jo kuitattu yllä.
            continue;
        };
        if text.is_empty() {
            continue;
        }

        // conversation = chat.id (invariantti #4: vastausosoite säilyy).
        let Some(chat_id) = msg
            .get("chat")
            .and_then(|c| c.get("id"))
            .and_then(serde_json::Value::as_i64)
        else {
            continue;
        };

        // sender = from.id; fallback chat.id jos 'from' puuttuu (esim.
        // kanavaviestit). Tyhjää lähettäjää ei sallita (InboundMessage::new).
        let sender_id = msg
            .get("from")
            .and_then(|fr| fr.get("id"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(chat_id);

        // Yksittäinen kelvoton viesti ei kaada koko kierrosta — ohitetaan.
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
        // Trailing slash on jo trimmattu; polku on /bot<token>/<method>.
        let url = TelegramChannel::method_url(&ch.inner, "getUpdates");
        assert_eq!(url, "https://example.test/botTKN/getUpdates");
    }

    #[test]
    fn from_env_errors_when_unset() {
        // Varmista että muuttuja ei ole asetettu tässä testissä.
        std::env::remove_var(TOKEN_ENV);
        assert!(TelegramChannel::from_env("tg-1").is_err());
    }

    // --- parse_get_updates: long-poll-parsinta (ei verkkoa) ---

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
        // conversation = chat.id (invariantti #4)
        assert_eq!(m.conversation, "-1009");
        assert_eq!(m.body, "moi");
        // offset-kuittaus: max(update_id) + 1
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
        // max update_id = 7 → seuraava offset 8 (kuittaa 5,6,7).
        assert_eq!(outcome.next_offset, Some(8));
    }

    #[test]
    fn parse_empty_result_keeps_previous_offset() {
        let body = r#"{ "ok": true, "result": [] }"#;
        let outcome = parse_get_updates(body, Some(42)).expect("parse ok");
        assert!(outcome.messages.is_empty());
        // Ei uusia päivityksiä → edellinen offset säilyy.
        assert_eq!(outcome.next_offset, Some(42));
    }

    #[test]
    fn parse_non_text_update_is_acked_but_not_emitted() {
        // Sticker/kuva-päivitys ilman 'text'-kenttää: ei viestiä, mutta
        // update_id kuitataan jotta se ei tule uudelleen.
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
        // Kanavaviestissä 'from' voi puuttua → sender = chat.id.
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
        // update_id silti kuitataan (ettei tyhjä viesti jää loputtomaan looppiin).
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
        // Round-trip: parsittu InboundMessage → InboundEnvelope säilyttää
        // channel_id + conversation (invariantit #2 ja #4).
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
        // Reply ohjautuu takaisin samaan chattiin.
        let reply = env.reply("pong").expect("reply");
        assert_eq!(reply.target, "77");
    }
}
