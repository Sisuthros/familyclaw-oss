//! Kanavaviestien tyypit ja silta Resonance Busiin.
//!
//! Tämä moduuli määrittelee kolme tasoa:
//! - [`ChannelKind`] — mitä kanavateknologiaa viesti edustaa.
//! - [`OutboundMessage`] — alustalta ulospäin lähetettävä viesti.
//! - [`InboundMessage`] — kanavalta sisään saapunut raakaviesti.
//! - [`InboundEnvelope`] — kanonisoitu envelope, joksi saapuva viesti
//!   muunnetaan ennen kuin se julkaistaan Resonance Busiin.
//!
//! ## Miksi `InboundEnvelope` asuu täällä
//! Kanavakerros on Resonance Busin **reuna ulkomaailmaan**: se on se kerros,
//! joka tuottaa bus-viestit saapuvasta liikenteestä (`saapuva viesti →
//! InboundEnvelope → familyclaw_bus::BusMessage`, design §3).
//!
//! Tyyppi on tarkoituksella **erillinen** `familyclaw_bus::BusMessage`:sta
//! (busin hyötykuorma-enum): tämä on alkuperätietoinen *kirjekuori*
//! (kanava-id, lähettäjä, keskustelu), kun taas busin `BusMessage` on
//! sisältö-enum (teksti/tunnepulssi/latent/…). Nimet erotettiin, jotta
//! kaksi eri tyyppiä eivät enää jaa nimeä `BusMessage` yli crate-rajojen.
//! Varsinainen muunnos `InboundEnvelope → familyclaw_bus::BusMessage` tehdään
//! agent-kerroksessa (joka riippuu molemmista crateista), jotta kanavakerros
//! pysyy riippumattomana busin sisäisestä Ractor-toteutuksesta ja jotta
//! envelope on serde-sarjallistuva durable-replayta varten.

use familyclaw_core::{time, MessageId, Timestamp};
use serde::{Deserialize, Serialize};

/// Tuettu kanavateknologia.
///
/// Oikeat adapterit (serenity Discordille, teloxide Telegramille, …) ovat
/// craten feature-flagien takana; tämä enum kantaa vain tiedon siitä, mistä
/// kanavasta viesti tuli tai mihin se menee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChannelKind {
    /// Discord (adapteri `discord`-featuren takana, esim. serenity).
    Discord,
    /// Telegram (adapteri `telegram`-featuren takana, esim. teloxide).
    Telegram,
    /// `WhatsApp` (adapteri `whatsapp`-featuren takana).
    // Eksplisiittinen rename, jotta serde-muoto vastaa `as_str()`-arvoa
    // ("whatsapp"); `snake_case` tuottaisi muuten "whats_app".
    #[serde(rename = "whatsapp")]
    WhatsApp,
    /// Signal (adapteri `signal`-featuren takana).
    Signal,
    /// In-memory testikanava ([`crate::MockChannel`]) — ei ulkoista SDK:ta.
    Mock,
}

impl ChannelKind {
    /// Lyhyt, vakaa tunnistemerkkijono lokeja ja reititystä varten.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discord => "discord",
            Self::Telegram => "telegram",
            Self::WhatsApp => "whatsapp",
            Self::Signal => "signal",
            Self::Mock => "mock",
        }
    }

    /// Vaatiiko kanava ulkoisen kanava-SDK:n (ja siten feature-flagin).
    ///
    /// [`ChannelKind::Mock`] on ainoa joka toimii ilman ulkoista riippuvuutta.
    #[must_use]
    pub const fn requires_external_sdk(self) -> bool {
        !matches!(self, Self::Mock)
    }
}

impl std::fmt::Display for ChannelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Alustalta ulospäin lähetettävä viesti.
///
/// `target` on kanavakohtainen vastaanottaja-osoite (esim. Discord-kanavan
/// id, Telegram-chat-id). Kanava-adapteri tulkitsee sen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// Kanavakohtainen kohde (kanava-id, chat-id, puhelinnumero, …).
    pub target: String,
    /// Viestin tekstisisältö.
    pub body: String,
}

impl OutboundMessage {
    /// Rakentaa ulospäin lähetettävän viestin.
    ///
    /// # Errors
    /// [`crate::ChannelError::InvalidInput`] jos kohde tai sisältö on tyhjä.
    pub fn new(target: impl Into<String>, body: impl Into<String>) -> crate::ChannelResult<Self> {
        let target = target.into();
        let body = body.into();
        if target.trim().is_empty() {
            return Err(crate::ChannelError::invalid_input(
                "outbound target must not be empty",
            ));
        }
        if body.is_empty() {
            return Err(crate::ChannelError::invalid_input(
                "outbound body must not be empty",
            ));
        }
        Ok(Self { target, body })
    }
}

/// Kanavalta sisään saapunut raakaviesti, ennen bus-kanonisointia.
///
/// `sender` on kanavakohtainen lähettäjä-osoite (käyttäjä-id, puhelinnumero),
/// `conversation` on keskustelun/ryhmän/kanavan tunniste jonka sisällä viesti
/// saapui (käytetään vastaamiseen).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundMessage {
    /// Kanavakohtainen lähettäjän tunniste.
    pub sender: String,
    /// Keskustelun/ryhmän/kanavan tunniste (vastausosoite).
    pub conversation: String,
    /// Viestin tekstisisältö.
    pub body: String,
}

impl InboundMessage {
    /// Rakentaa saapuneen raakaviestin.
    ///
    /// # Errors
    /// [`crate::ChannelError::InvalidInput`] jos lähettäjä, keskustelu tai
    /// sisältö on tyhjä.
    pub fn new(
        sender: impl Into<String>,
        conversation: impl Into<String>,
        body: impl Into<String>,
    ) -> crate::ChannelResult<Self> {
        let sender = sender.into();
        let conversation = conversation.into();
        let body = body.into();
        if sender.trim().is_empty() {
            return Err(crate::ChannelError::invalid_input(
                "inbound sender must not be empty",
            ));
        }
        if conversation.trim().is_empty() {
            return Err(crate::ChannelError::invalid_input(
                "inbound conversation must not be empty",
            ));
        }
        if body.is_empty() {
            return Err(crate::ChannelError::invalid_input(
                "inbound body must not be empty",
            ));
        }
        Ok(Self {
            sender,
            conversation,
            body,
        })
    }

    /// Kanonisoi saapuneen viestin [`InboundEnvelope`]:ksi.
    ///
    /// `kind` ja `channel_id` kertovat mistä kanavasta viesti tuli. Uusi
    /// [`MessageId`] ja UTC-aikaleima liitetään mukaan, jotta bus ja durable-
    /// loki voivat viitata viestiin yksikäsitteisesti ja deterministisesti.
    #[must_use]
    pub fn into_envelope(
        self,
        kind: ChannelKind,
        channel_id: impl Into<String>,
    ) -> InboundEnvelope {
        InboundEnvelope {
            id: MessageId::new(),
            kind,
            channel_id: channel_id.into(),
            sender: self.sender,
            conversation: self.conversation,
            body: self.body,
            received_at: time::now(),
        }
    }
}

/// Kanonisoitu, alkuperätietoinen viesti-kirjekuori joka virtaa kohti
/// Resonance Busia.
///
/// Tämä on se muoto, jonka kanavakerros tuottaa saapuvasta liikenteestä. Se on
/// täysin serde-sarjallistuva durable-replayta varten ja sisältää
/// alkuperätiedot ([`ChannelKind`], `channel_id`, `sender`, `conversation`),
/// jotta vastaus voidaan reitittää takaisin oikealle kanavalle.
///
/// **Huom:** tämä on eri tyyppi kuin `familyclaw_bus::BusMessage` (busin
/// sisältö-enum). Muunnos busin hyötykuormaksi tehdään agent-kerroksessa,
/// joka riippuu molemmista crateista.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundEnvelope {
    /// Viestin yksikäsitteinen tunniste busissa.
    pub id: MessageId,
    /// Kanavatyyppi josta viesti saapui.
    pub kind: ChannelKind,
    /// Sen konkreettisen kanavainstanssin tunniste, jolta viesti saapui
    /// (vastaa [`crate::Channel::channel_id`]-arvoa).
    pub channel_id: String,
    /// Kanavakohtainen lähettäjän tunniste.
    pub sender: String,
    /// Keskustelun/ryhmän tunniste (vastausosoite).
    pub conversation: String,
    /// Viestin tekstisisältö.
    pub body: String,
    /// Vastaanottohetki UTC:ssä.
    pub received_at: Timestamp,
}

impl InboundEnvelope {
    /// Rakentaa [`OutboundMessage`]-vastauksen tähän viestiin annetulla
    /// sisällöllä. Vastaus ohjautuu takaisin samaan keskusteluun.
    ///
    /// # Errors
    /// [`crate::ChannelError::InvalidInput`] jos vastaussisältö on tyhjä.
    pub fn reply(&self, body: impl Into<String>) -> crate::ChannelResult<OutboundMessage> {
        OutboundMessage::new(self.conversation.clone(), body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_kind_str_and_sdk_flag() {
        assert_eq!(ChannelKind::Discord.as_str(), "discord");
        assert_eq!(ChannelKind::Telegram.as_str(), "telegram");
        assert_eq!(ChannelKind::WhatsApp.as_str(), "whatsapp");
        assert_eq!(ChannelKind::Signal.as_str(), "signal");
        assert_eq!(ChannelKind::Mock.as_str(), "mock");
        assert_eq!(ChannelKind::Discord.to_string(), "discord");

        assert!(ChannelKind::Discord.requires_external_sdk());
        assert!(!ChannelKind::Mock.requires_external_sdk());
    }

    #[test]
    fn channel_kind_serde_is_snake_case() {
        let json = serde_json::to_string(&ChannelKind::WhatsApp).expect("serialize");
        assert_eq!(json, "\"whatsapp\"");
        let back: ChannelKind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, ChannelKind::WhatsApp);
    }

    #[test]
    fn channel_kind_serde_matches_as_str_for_all_variants() {
        // Lukitse invariantti: serde-muoto == as_str() jokaiselle variantille,
        // jotta lokit ja sarjallistus eivät eriydy.
        for kind in [
            ChannelKind::Discord,
            ChannelKind::Telegram,
            ChannelKind::WhatsApp,
            ChannelKind::Signal,
            ChannelKind::Mock,
        ] {
            let json = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            let back: ChannelKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn outbound_new_validates() {
        assert!(OutboundMessage::new("c", "hi").is_ok());
        assert!(OutboundMessage::new("  ", "hi").is_err());
        assert!(OutboundMessage::new("c", "").is_err());
    }

    #[test]
    fn inbound_new_validates() {
        assert!(InboundMessage::new("u", "room", "hi").is_ok());
        assert!(InboundMessage::new("", "room", "hi").is_err());
        assert!(InboundMessage::new("u", " ", "hi").is_err());
        assert!(InboundMessage::new("u", "room", "").is_err());
    }

    #[test]
    fn inbound_into_envelope_carries_origin() {
        let inbound = InboundMessage::new("user42", "general", "hello").expect("valid");
        let env = inbound.into_envelope(ChannelKind::Discord, "discord-main");
        assert_eq!(env.kind, ChannelKind::Discord);
        assert_eq!(env.channel_id, "discord-main");
        assert_eq!(env.sender, "user42");
        assert_eq!(env.conversation, "general");
        assert_eq!(env.body, "hello");
        assert!(!env.id.is_nil());
    }

    #[test]
    fn distinct_envelopes_get_distinct_ids() {
        let a = InboundMessage::new("u", "r", "x")
            .expect("valid")
            .into_envelope(ChannelKind::Mock, "m");
        let b = InboundMessage::new("u", "r", "x")
            .expect("valid")
            .into_envelope(ChannelKind::Mock, "m");
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn envelope_reply_targets_conversation() {
        let env = InboundMessage::new("u", "room-7", "ping")
            .expect("valid")
            .into_envelope(ChannelKind::Telegram, "tg-1");
        let reply = env.reply("pong").expect("valid reply");
        assert_eq!(reply.target, "room-7");
        assert_eq!(reply.body, "pong");

        assert!(env.reply("").is_err());
    }

    #[test]
    fn envelope_serde_roundtrip() {
        let env = InboundMessage::new("u", "r", "body")
            .expect("valid")
            .into_envelope(ChannelKind::Signal, "sig-1");
        let json = serde_json::to_string(&env).expect("serialize");
        let back: InboundEnvelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(env, back);
    }
}
