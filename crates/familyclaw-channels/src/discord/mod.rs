//! [`DiscordChannel`] — Discord-adapteri serenity-gatewayllä.
//!
//! Tämä moduuli toteuttaa [`Channel`]-rajapinnan Discordille käyttäen
//! **serenity 0.12** -kirjastoa. Koko toteutus on feature-gated (`discord`),
//! jotta oletuskäännökseen ei vedetä raskasta gateway-WebSocket-SDK:ta ilman
//! tarvetta.
//!
//! ## Rakenne
//! - [`DiscordChannel`] omistaa kohdekanavan id:n, jaetun `Arc<Http>`-asiakkaan
//!   lähetyksiä varten sekä saapuvan virran mpsc-päät.
//! - [`DiscordChannel::start`] käynnistää serenity-gatewayn taustatehtävänä ja
//!   palaa vasta kun yhteys on `ready` tai se epäonnistuu (ei niele virheitä).
//! - [`Channel::receive`] luovuttaa saapuvan virran **kerran**: gatewayn
//!   vastaanottamat viestit ohjataan `inbound_tx`:n kautta tähän virtaan.
//! - [`Channel::send`] lähettää viestin Discordin REST-rajapinnalla `Arc<Http>`:n
//!   kautta, pilkottuna Discordin 2000 merkin rajaan ([`split::split_message`]).
//! - [`DiscordChannel::stop`] sulkee gatewayn siististi (`shutdown_all`).
//!
//! ## Apumoduulit (raita B)
//! Saapuvan viestin suodatus/muunnos ([`map::map_message`]) ja lähtevän viestin
//! pilkonta ([`split::split_message`]) ovat serenity-riippumattomissa
//! alimoduuleissa, jotta niiden logiikka on yksikkötestattavissa ilman
//! gateway-kontekstia.
//!
//! ## `MESSAGE_CONTENT` -intent (privileged)
//! Botti EI saa viestien tekstisisältöä ilman [`GatewayIntents::MESSAGE_CONTENT`]
//! -intentiä. Sen lisäksi intent on aktivoitava **Discord Developer Portalissa**
//! (Bot → Privileged Gateway Intents → Message Content Intent). Ilman tätä
//! `msg.content` on tyhjä kaikissa guild-viesteissä ja ne suodattuvat pois.
//!
//! ## KERROS A -säännöt
//! Kaikki konfiguraatio (`bot_token`, `target_channel_id`) on ajonaikaista — ei
//! kovakoodattuja arvoja. Tokenia ei koskaan lokiteta eikä se päädy
//! `Debug`-tulosteeseen (ks. [`std::fmt::Debug`]-toteutus).

pub mod map;
pub mod split;

use std::sync::Arc;
use std::time::Duration;

use serenity::all::{ChannelId, CreateMessage, GatewayIntents, Message, Ready};
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::gateway::ShardManager;
use serenity::http::Http;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, info, warn};

use crate::channel::{Channel, MessageStream, SendFuture};
use crate::error::{ChannelError, ChannelResult};
use crate::message::{ChannelKind, InboundEnvelope, OutboundMessage};

use map::map_message;
use split::split_message;

/// Discordin viestin enimmäispituus merkkeinä (Unicode scalar -määrä).
const DISCORD_MAX_MESSAGE_CHARS: usize = 2000;

/// Kuinka kauan [`DiscordChannel::start`] odottaa `ready`-tapahtumaa ennen kuin
/// se luovuttaa ja palauttaa virheen (esim. väärä token jää muuten roikkumaan).
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Discord-kanava — toteuttaa [`Channel`]-rajapinnan serenity-gatewayllä.
///
/// Kaikki asetukset (token, `target_channel_id`) annetaan rakennettaessa.
/// Mikään arvo ei ole kovakoodattu.
pub struct DiscordChannel {
    /// Tämän kanavainstanssin vakaa tunniste (`discord-<id>`).
    channel_id: String,
    /// Discord bot token (ladataan runtime-konfigista, ei kovakoodata).
    bot_token: String,
    /// Kohdekanavan Discord-id.
    target_channel_id: u64,
    /// Jaettu HTTP-asiakas REST-lähetyksiin. Luodaan **kerran** konstruktorissa.
    http: Arc<Http>,
    /// Saapuvan virran vastaanotin; luovutetaan **kerran** [`Channel::receive`]:ssä.
    inbound_rx: Mutex<Option<mpsc::UnboundedReceiver<InboundEnvelope>>>,
    /// Lähetin saapuville viesteille; kloonataan gateway-handlerille
    /// [`DiscordChannel::start`]:ssa.
    inbound_tx: mpsc::UnboundedSender<InboundEnvelope>,
    /// Käynnissä olevan gatewayn `ShardManager`; asetetaan
    /// [`DiscordChannel::start`]:ssa, käytetään [`DiscordChannel::stop`]:ssa
    /// graceful shutdowniin.
    shard_manager: Mutex<Option<Arc<ShardManager>>>,
}

impl DiscordChannel {
    /// Luo uuden Discord-kanavan.
    ///
    /// # Arguments
    /// * `bot_token` — Discord bot token (ladataan env:stä, ei kovakoodata).
    /// * `target_channel_id` — kohdekanavan id Discordissa (ei saa olla 0).
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] jos token on tyhjä tai `target_channel_id`
    /// on 0.
    pub fn new(bot_token: impl Into<String>, target_channel_id: u64) -> ChannelResult<Self> {
        let bot_token = bot_token.into();

        if bot_token.trim().is_empty() {
            return Err(ChannelError::invalid_input("bot_token must not be empty"));
        }
        if target_channel_id == 0 {
            return Err(ChannelError::invalid_input(
                "target_channel_id must not be 0",
            ));
        }

        let channel_id = format!("discord-{target_channel_id}");
        // Yksi Http-asiakas koko kanavan eliniäksi (jaetaan Arc:lla send-kutsuille).
        let http = Arc::new(Http::new(&bot_token));
        // YKSI mpsc-pari koko kanavan eliniäksi: tx menee gatewaylle, rx
        // luovutetaan receive()-kutsujalle. Tämä korjaa vian, jossa vanha
        // toteutus pudotti vastaanottimen konstruktorissa (viestejä ei saatu).
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();

        Ok(Self {
            channel_id,
            bot_token,
            target_channel_id,
            http,
            inbound_rx: Mutex::new(Some(inbound_rx)),
            inbound_tx,
            shard_manager: Mutex::new(None),
        })
    }

    /// Käynnistää Discord-gateway-yhteyden taustatehtävänä ja palaa vasta kun
    /// yhteys on `ready` tai käynnistys epäonnistuu.
    ///
    /// Saapuvat viestit ohjataan `inbound_tx`-kanavaan ja sieltä
    /// [`Channel::receive`]-virtaan. `start()` ei niele gateway-virheitä: jos
    /// token on väärä tai yhteys katkeaa heti, kutsu palauttaa virheen
    /// `READY_TIMEOUT`:n sisällä sen sijaan että jäisi hiljaa roikkumaan.
    ///
    /// # Errors
    /// [`ChannelError::Backend`] jos serenity-clientin rakennus epäonnistuu,
    /// gateway-tehtävä kaatuu ennen valmiutta tai `ready` ei saavu ajoissa.
    pub async fn start(&self) -> ChannelResult<()> {
        // MESSAGE_CONTENT on privileged-intent: ilman sitä msg.content on tyhjä
        // (ks. moduulidokumentaatio). Aktivoitava myös Developer Portalissa.
        let intents = GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT;

        let (ready_tx, ready_rx) = oneshot::channel::<()>();
        let handler = DiscordHandler {
            target_channel_id: self.target_channel_id,
            inbound_tx: self.inbound_tx.clone(),
            ready_tx: Mutex::new(Some(ready_tx)),
        };

        let mut client = Client::builder(&self.bot_token, intents)
            .event_handler(handler)
            .await
            .map_err(|e| ChannelError::backend(&self.channel_id, e.to_string()))?;

        // Talleta shard_manager stop()-kutsua varten.
        *self.shard_manager.lock().await = Some(client.shard_manager.clone());

        // Käynnistä client taustatehtävänä. Virhe välitetään takaisin
        // err-kanavan kautta, jotta start() ei niele sitä (T4).
        let (err_tx, err_rx) = oneshot::channel::<ChannelError>();
        let channel_label = self.channel_id.clone();
        tokio::spawn(async move {
            if let Err(e) = client.start().await {
                // Lähetys voi epäonnistua jos start() on jo palannut ready-polulta;
                // se on ok (gateway oli jo valmis, virhe tuli vasta myöhemmin).
                let _ = err_tx.send(ChannelError::backend(&channel_label, e.to_string()));
            }
        });

        // Odota ENSIMMÄISTÄ tapahtumaa: ready, varhainen virhe tai timeout.
        tokio::select! {
            res = ready_rx => {
                match res {
                    Ok(()) => {
                        info!(channel = %self.channel_id, "Discord gateway ready");
                        Ok(())
                    }
                    // ready_tx pudotettiin ilman signaalia → handler katosi.
                    Err(_) => Err(ChannelError::backend(
                        &self.channel_id,
                        "gateway handler dropped before ready",
                    )),
                }
            }
            err = err_rx => {
                match err {
                    Ok(e) => Err(e),
                    Err(_) => Err(ChannelError::backend(
                        &self.channel_id,
                        "gateway task ended before ready",
                    )),
                }
            }
            () = tokio::time::sleep(READY_TIMEOUT) => {
                Err(ChannelError::backend(
                    &self.channel_id,
                    format!("gateway did not become ready within {}s", READY_TIMEOUT.as_secs()),
                ))
            }
        }
    }

    /// Sulkee gateway-yhteyden siististi.
    ///
    /// Idempotentti: jos gatewaytä ei ole käynnistetty tai se on jo suljettu,
    /// kutsu palaa `Ok`-tilassa.
    ///
    /// # Errors
    /// Ei palauta virhettä normaalitilanteessa; allekirjoitus säilyttää
    /// [`ChannelResult`]-muodon symmetrian [`DiscordChannel::start`]:n kanssa.
    pub async fn stop(&self) -> ChannelResult<()> {
        if let Some(manager) = self.shard_manager.lock().await.take() {
            manager.shutdown_all().await;
            info!(channel = %self.channel_id, "Discord gateway stopped");
        } else {
            debug!(channel = %self.channel_id, "Discord stop() called but gateway not running");
        }
        Ok(())
    }

    /// Rakentaa Discord-kanavan **webhook/HTTP-inbound-polkua** varten (gatewayn
    /// `/inject` + `/discord/interactions`-reitit). Tässä mallissa saapuva
    /// liikenne tulee [`DiscordChannel::inject`]:n kautta — serenity-gatewaytä ei
    /// käynnistetä [`DiscordChannel::start`]:lla.
    ///
    /// `channel_id` on tämän kanavainstanssin vakaa tunniste (esim.
    /// `"discord-main"`), ei välttämättä Discord-snowflake. Jos se on numeerinen,
    /// se talletetaan myös `target_channel_id`:ksi lähetyksiä varten; muuten
    /// lähetys kulkee bus-pumpun kautta (`inject`/`receive`).
    ///
    /// # Errors
    /// [`ChannelError::InvalidInput`] jos `webhook_url` tai `channel_id` on tyhjä.
    pub fn from_webhook(
        webhook_url: impl Into<String>,
        channel_id: impl Into<String>,
    ) -> ChannelResult<Self> {
        let webhook_url = webhook_url.into();
        let channel_id = channel_id.into();

        if webhook_url.trim().is_empty() {
            return Err(ChannelError::invalid_input("webhook_url must not be empty"));
        }
        if channel_id.trim().is_empty() {
            return Err(ChannelError::invalid_input("channel_id must not be empty"));
        }

        // Numeerinen channel_id → snowflake lähetyksiä varten; muuten 0
        // (webhook-only, lähetys kulkee bus-pumpun läpi).
        let target_channel_id = channel_id
            .trim_start_matches("discord-")
            .parse::<u64>()
            .unwrap_or(0);
        // Http-asiakas luodaan webhook-urlilla; sitä käytetään vain jos
        // target_channel_id on aito snowflake.
        let http = Arc::new(Http::new(&webhook_url));
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();

        Ok(Self {
            channel_id,
            bot_token: webhook_url,
            target_channel_id,
            http,
            inbound_rx: Mutex::new(Some(inbound_rx)),
            inbound_tx,
            shard_manager: Mutex::new(None),
        })
    }

    /// Injektoi valmiin [`InboundEnvelope`]:n saapuvaan virtaan.
    ///
    /// Käytetään HTTP-inbound-poluissa (`/inject`, `/discord/interactions`):
    /// työntää envelopen **samaan** `inbound_tx`:ään jota serenity-handler ja
    /// [`Channel::receive`]-virta käyttävät — injektoidut ja gatewayn
    /// vastaanottamat viestit jakavat yhden virran.
    ///
    /// # Errors
    /// [`ChannelError::Receive`] jos vastaanotin on suljettu (stream pudotettu).
    pub fn inject(&self, envelope: InboundEnvelope) -> ChannelResult<()> {
        self.inbound_tx
            .send(envelope)
            .map_err(|e| ChannelError::receive(&self.channel_id, e.to_string()))
    }

    /// Pilkkoo lähtevän viestin Discordin merkkirajaan ja lähettää palat
    /// järjestyksessä `Arc<Http>`:n kautta.
    async fn send_body(
        http: &Http,
        channel: ChannelId,
        channel_id: &str,
        body: &str,
    ) -> ChannelResult<()> {
        // split_message returns an EMPTY Vec for an empty/whitespace-only body.
        // Without this guard the loop sends nothing yet still logs "message sent"
        // — a silent drop that lies in the logs. Reject it with a clear error so
        // an empty outbound is impossible to mistake for success.
        let chunks = split_message(body, DISCORD_MAX_MESSAGE_CHARS);
        if chunks.is_empty() {
            return Err(ChannelError::invalid_input(format!(
                "refusing to send empty/whitespace-only message body to '{channel_id}'"
            )));
        }
        for chunk in chunks {
            let message = CreateMessage::new().content(chunk);
            channel
                .send_message(http, message)
                .await
                .map_err(|e| map_send_error(channel_id, &e))?;
        }
        debug!(channel = %channel_id, "message sent to Discord");
        Ok(())
    }
}

/// Muuntaa serenity-lähetysvirheen [`ChannelError`]:ksi siten, että
/// uudelleenyritettävyys (rate-limit / palvelinvirhe) erottuu pysyvistä
/// konfiguraatiovirheistä (väärä token / puuttuva pääsy).
fn map_send_error(channel_id: &str, err: &serenity::Error) -> ChannelError {
    if let serenity::Error::Http(http_err) = err {
        if let Some(status) = http_err.status_code() {
            let code = status.as_u16();
            // 401/403/404 = pysyvä: token/oikeudet/kanava väärin → ei uudelleenyritystä.
            if matches!(code, 401 | 403 | 404) {
                return ChannelError::backend(
                    channel_id,
                    format!("permanent HTTP {code}: {http_err}"),
                );
            }
        }
    }
    // 429 / 5xx / verkkovirhe = väliaikainen → uudelleenyritettävä (Send).
    ChannelError::send(channel_id, err.to_string())
}

/// Discord-tapahtumakäsittelijä: ohjaa kohdekanavan viestit `inbound_tx`:ään ja
/// signaloi valmiuden `ready_tx`:llä.
struct DiscordHandler {
    target_channel_id: u64,
    inbound_tx: mpsc::UnboundedSender<InboundEnvelope>,
    /// Kertasignaali [`DiscordChannel::start`]:lle kun gateway on `ready`.
    ready_tx: Mutex<Option<oneshot::Sender<()>>>,
}

#[async_trait]
impl EventHandler for DiscordHandler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        info!(
            bot = %ready.user.name,
            guilds = ready.guilds.len(),
            "Discord gateway connected"
        );
        // Signaloi start():lle (kerran). Lukko otetaan ja vapautetaan tässä
        // lohkossa ennen funktion paluuta; deadlock-riskiä ei ole.
        if let Some(tx) = self.ready_tx.lock().await.take() {
            let _ = tx.send(());
        }
    }

    async fn message(&self, _ctx: Context, msg: Message) {
        // Suodatus ja muunnos delegoidaan puhtaalle funktiolle (raita B), jotta
        // logiikka on testattavissa ilman serenity-kontekstia.
        let Some(envelope) = map_message(
            msg.author.id.get(),
            msg.author.bot,
            msg.channel_id.get(),
            self.target_channel_id,
            &msg.content,
        ) else {
            return;
        };

        if let Err(e) = self.inbound_tx.send(envelope) {
            warn!(error = %e, "failed to forward inbound Discord envelope (receiver dropped?)");
        }
    }
}

impl std::fmt::Debug for DiscordChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Token EI päädy lokeihin/Debug-tulosteeseen (KERROS A: ei salaisuuksia).
        f.debug_struct("DiscordChannel")
            .field("channel_id", &self.channel_id)
            .field("target_channel_id", &self.target_channel_id)
            .field("bot_token", &"***")
            .finish_non_exhaustive()
    }
}

impl Channel for DiscordChannel {
    fn channel_id(&self) -> &str {
        &self.channel_id
    }

    fn kind(&self) -> ChannelKind {
        ChannelKind::Discord
    }

    fn send(&self, message: OutboundMessage) -> SendFuture<'_> {
        let http = Arc::clone(&self.http);
        let target_id = self.target_channel_id;
        let channel_id = self.channel_id.clone();
        Box::pin(async move {
            // target_channel_id == 0 tarkoittaa webhook/inbound-only-instanssia
            // (`from_webhook` ei-numeerisella channel_id:llä): lähtevälle viestille
            // ei ole aitoa Discord-snowflakea. Palauta SELKEÄ virhe sen sijaan että
            // yrittäisi lähettää kanavalle 0 (joka epäonnistuisi hämärästi).
            // Lähtevä liikenne kulkee tällöin bus-pumpun / oikean send-kanavan kautta.
            if target_id == 0 {
                return Err(ChannelError::invalid_input(format!(
                    "channel '{channel_id}' is inbound-only (no numeric Discord channel id); \
                     outbound send is not supported on a webhook-only instance — \
                     construct DiscordChannel::new(bot_token, target_channel_id) for sending"
                )));
            }
            let target = ChannelId::new(target_id);
            Self::send_body(&http, target, &channel_id, &message.body).await
        })
    }

    fn receive(&self) -> ChannelResult<MessageStream> {
        // Ota talletettu rx KERRAN. Toinen kutsu → selkeä virhe (sama yksi
        // mpsc-pari kuin start() käyttää; viestit eivät katoa irralliseen kanavaan).
        let rx = self
            .inbound_rx
            .try_lock()
            .map_err(|_| ChannelError::backend(self.channel_id(), "inbound_rx lock contended"))?
            .take()
            .ok_or_else(|| {
                ChannelError::receive(self.channel_id(), "receive stream already taken")
            })?;
        Ok(MessageStream::new(rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_discord_channel_validates_tokens() {
        assert!(DiscordChannel::new("", 123_456).is_err());
        assert!(DiscordChannel::new("  ", 123_456).is_err());
        assert!(DiscordChannel::new("valid_token", 0).is_err());
        assert!(DiscordChannel::new("valid_token", 123_456).is_ok());
    }

    #[test]
    fn channel_id_and_kind() {
        let ch = DiscordChannel::new("test_token", 987_654).expect("channel");
        assert_eq!(ch.channel_id(), "discord-987654");
        assert_eq!(ch.kind(), ChannelKind::Discord);
    }

    #[test]
    fn debug_does_not_leak_token() {
        let ch = DiscordChannel::new("SECRET-TOKEN-123", 555).expect("channel");
        let dbg = format!("{ch:?}");
        assert!(dbg.contains("DiscordChannel"));
        assert!(dbg.contains("discord-555"));
        assert!(
            !dbg.contains("SECRET-TOKEN-123"),
            "token must not appear in Debug output"
        );
    }

    #[tokio::test]
    async fn receive_twice_returns_error() {
        let ch = DiscordChannel::new("token", 123).expect("channel");
        assert!(ch.receive().is_ok(), "first receive() yields the stream");
        assert!(
            ch.receive().is_err(),
            "second receive() must error (stream already taken)"
        );
    }

    #[tokio::test]
    async fn inbound_tx_reaches_receive_stream() {
        let ch = DiscordChannel::new("token", 777).expect("channel");
        let mut stream = ch.receive().expect("stream");

        // Simuloi gatewayn vastaanottama viesti raita B:n map_message-funktion
        // kautta ja työnnä se inbound_tx:ään (sama polku kuin EventHandler::message).
        let env = map_message(42, false, 777, 777, "hei").expect("valid");
        ch.inbound_tx.send(env).expect("send to inbound");

        let got = stream.recv().await.expect("one message");
        assert_eq!(got.body, "hei");
        assert_eq!(got.kind, ChannelKind::Discord);
    }

    #[tokio::test]
    async fn stop_without_start_is_ok() {
        let ch = DiscordChannel::new("token", 1).expect("channel");
        assert!(
            ch.stop().await.is_ok(),
            "stop() before start() is idempotent"
        );
    }

    #[test]
    fn from_webhook_rejects_empty_webhook_url() {
        assert!(DiscordChannel::from_webhook("", "discord-main").is_err());
        assert!(DiscordChannel::from_webhook("   ", "discord-main").is_err());
    }

    #[test]
    fn from_webhook_rejects_empty_channel_id() {
        assert!(DiscordChannel::from_webhook("https://example.invalid/wh", "").is_err());
        assert!(DiscordChannel::from_webhook("https://example.invalid/wh", "  ").is_err());
    }

    #[test]
    fn from_webhook_parses_discord_prefixed_snowflake() {
        // "discord-<snowflake>" → prefiksi karsitaan, numero parsitaan target_channel_id:ksi.
        let ch = DiscordChannel::from_webhook("https://example.invalid/wh", "discord-123456")
            .expect("channel");
        assert_eq!(ch.channel_id(), "discord-123456");
        assert_eq!(ch.target_channel_id, 123_456);
    }

    #[test]
    fn from_webhook_parses_bare_numeric_channel_id() {
        // Pelkkä numeerinen id (ilman prefiksiä) parsitaan suoraan.
        let ch =
            DiscordChannel::from_webhook("https://example.invalid/wh", "987654").expect("channel");
        assert_eq!(ch.channel_id(), "987654");
        assert_eq!(ch.target_channel_id, 987_654);
    }

    #[test]
    fn from_webhook_non_numeric_channel_id_defaults_target_to_zero() {
        // Ei-numeerinen id (esim. nimetty kanava) → target_channel_id = 0 (webhook-only).
        let ch = DiscordChannel::from_webhook("https://example.invalid/wh", "discord-main")
            .expect("channel");
        assert_eq!(ch.channel_id(), "discord-main");
        assert_eq!(ch.target_channel_id, 0);
        assert_eq!(ch.kind(), ChannelKind::Discord);
    }

    #[tokio::test]
    async fn send_on_inbound_only_webhook_returns_clear_error() {
        // Webhook-only-instanssi (target_channel_id == 0) ei voi lähettää: send()
        // palauttaa SELKEÄN InvalidInput-virheen sen sijaan että yrittäisi
        // hämärästi lähettää Discord-kanavalle 0. (P1: outbound impossible to misunderstand.)
        let ch = DiscordChannel::from_webhook("https://example.invalid/wh", "discord-main")
            .expect("channel");
        assert_eq!(ch.target_channel_id, 0);

        let err = ch
            .send(OutboundMessage::new("discord-main", "hello").expect("msg"))
            .await
            .expect_err("inbound-only webhook must reject outbound send");

        assert!(
            matches!(err, ChannelError::InvalidInput(_)),
            "expected InvalidInput, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("inbound-only"),
            "error must explain inbound-only nature, got: {msg}"
        );
    }

    #[test]
    fn from_webhook_debug_does_not_leak_url() {
        // bot_token-kenttään talletettu webhook_url ei saa näkyä Debug-tulosteessa.
        let ch =
            DiscordChannel::from_webhook("https://example.invalid/SECRET-WEBHOOK", "discord-main")
                .expect("channel");
        let dbg = format!("{ch:?}");
        assert!(dbg.contains("DiscordChannel"));
        assert!(
            !dbg.contains("SECRET-WEBHOOK"),
            "webhook url must not appear in Debug output"
        );
    }
}
