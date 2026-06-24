//! Sessio-isolaatio (F4) — per-viesti alkuperä ([`MessageOrigin`]) ja siitä
//! johdettu [`MessageOrigin::session_key`].
//!
//! ## Miksi tämä moduuli on olemassa
//! FamilyClaw-MVP ajaa **yhtä** agenttia, **yhtä** muistia ja **staattista**
//! reply-kohdetta ([`Agent::with_reply_target`](crate::Agent::with_reply_target)).
//! Tämä on oikein tasan silloin kun on yksi kanava ja yksi keskustelu. Heti kun
//! kanavia tai keskusteluja on enemmän kuin yksi, kaikki keskustelut vuotavat
//! samaan kontekstiin ja muistiin: agentti sekoittaa A:n ja B:n. Sessio-isolaatio
//! erottaa keskustelut **session-avaimella**.
//!
//! ## Mikä on `session_key`
//! `session_key = "<channel_id>:<conversation>"`. Tämä on luonteva avain:
//! sama kanava + sama keskustelu = sama sessio; eri keskustelu = eri sessio.
//! Lähettäjä ([`MessageOrigin::sender`]) kulkee mukana auditointia varten, mutta
//! **ei** ole osa session-avainta (sama keskustelu voi olla monenkeskinen).
//!
//! ## Suhde F2-origin-sopimukseen (RIIPPUVUUS)
//! [`MessageOrigin`] on F4:n **rajapinta**, joka odottaa F2-origin-sopimusta
//! (origin-kenttä bus-kirjekuoressa [`ResonanceMessage`](familyclaw_bus::ResonanceMessage)).
//! Kanavakerros tuottaa jo täsmälleen tarvittavat kentät
//! ([`InboundEnvelope`](familyclaw_channels::InboundEnvelope): `channel_id`,
//! `conversation`, `sender`); [`MessageOrigin::from_inbound_envelope`] kuvaa ne
//! suoraan. Kun F2 vie originin bus-kirjekuoreen ja
//! [`Agent::handle_turn`](crate::Agent::handle_turn) saa sen per-viesti, tämä
//! tyyppi on valmis kytkettäväksi ilman uutta suunnittelua.
//!
//! ## Mitä F4 tekee kun origin on kytketty (dokumentoitu toteutusreitti)
//! Yksi agentti, yksi muisti — **ei** per-sessio Agent-instansseja (ylirakennus).
//! Isolaatio tehdään muisti-scopella session-avaimella:
//! 1. **Kirjoitus:** [`Agent::handle_turn`](crate::Agent::handle_turn) liittää
//!    muistoon tagin `session:<key>` (origin-Some-haara), [`session_tag`](MessageOrigin::session_tag).
//! 2. **Luku:** [`Agent::think`](crate::Agent::think) suodattaa recallin samalla
//!    `session:<key>`-tagilla → A:n muistot eivät vuoda B:n kontekstiin.
//! 3. **Reply-kohde:** vastaus johdetaan originin keskustelusta, ei staattisesta
//!    reply-kohteesta — origin ENSIN, fallback staattiseen
//!    ([`MessageOrigin::reply_target`]).
//!
//! Vaihe 2 (recall-suodatus) odottaa muistikerroksen tag-filtteriä; siihen asti
//! `session:<key>`-tag kirjoitetaan jo nyt (vaihe 1 + 3 ovat valmiita), ja luku
//! suodattaa kun rajapinta on saatavilla. Ks. [`session_tag`](MessageOrigin::session_tag).
//!
//! ## OSS-raja (KERROS A)
//! Geneeristä alustakoodia: ei kovakoodattuja kanavanimiä, keskusteluja,
//! avaimia eikä polkuja. Kaikki alkuperätieto tulee ajonaikaisesti viestistä.

use serde::{Deserialize, Serialize};

/// Tag-etuliite session-scopatuille muistoille. Muisto tagataan
/// `"<SESSION_TAG_PREFIX><session_key>"`:llä kirjoitettaessa, ja recall
/// suodatetaan samalla tagilla luettaessa — näin eri sessioiden muistot eivät
/// vuoda toistensa kontekstiin.
pub const SESSION_TAG_PREFIX: &str = "session:";

/// Yhden saapuvan viestin **alkuperä** (F2-origin-sopimuksen muoto): mistä
/// kanavasta, mistä keskustelusta ja keneltä viesti tuli.
///
/// Kaikki kentät ovat [`String`], joten tyyppi on suoraan serde-sarjallistuva
/// (durable-replay + bus-kirjekuoren `origin`-kenttä, kun F2 vie sen sinne).
///
/// ## Kentät
/// - `channel_id` — kanavainstanssin tunniste (esim. `"discord-main"`),
///   vastaa [`InboundEnvelope::channel_id`](familyclaw_channels::InboundEnvelope).
/// - `conversation` — keskustelun/ryhmän tunniste (vastausosoite),
///   vastaa [`InboundEnvelope::conversation`](familyclaw_channels::InboundEnvelope).
/// - `sender` — kanavakohtainen lähettäjän tunniste (auditointiin; **ei** osa
///   session-avainta), vastaa
///   [`InboundEnvelope::sender`](familyclaw_channels::InboundEnvelope).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageOrigin {
    /// Kanavainstanssin tunniste (session-avaimen ensimmäinen osa).
    pub channel_id: String,
    /// Keskustelun/ryhmän tunniste (session-avaimen toinen osa + reply-kohde).
    pub conversation: String,
    /// Kanavakohtainen lähettäjän tunniste (auditointi; ei session-avaimessa).
    pub sender: String,
}

impl MessageOrigin {
    /// Rakentaa alkuperän paljaista osista.
    #[must_use]
    pub fn new(
        channel_id: impl Into<String>,
        conversation: impl Into<String>,
        sender: impl Into<String>,
    ) -> Self {
        Self {
            channel_id: channel_id.into(),
            conversation: conversation.into(),
            sender: sender.into(),
        }
    }

    /// Johtaa **session-avaimen** alkuperästä: `"<channel_id>:<conversation>"`.
    ///
    /// Tämä on F4:n ydin: kaksi viestiä kuuluvat samaan sessioon **joss** ne
    /// tulivat samasta kanavasta ja samasta keskustelusta. Lähettäjä ei vaikuta
    /// avaimeen (monenkeskinen keskustelu jakaa session).
    #[must_use]
    pub fn session_key(&self) -> String {
        format!("{}:{}", self.channel_id, self.conversation)
    }

    /// Johtaa **muisti-tagin** session-avaimesta: `"session:<channel_id>:<conversation>"`.
    ///
    /// [`Agent::handle_turn`](crate::Agent::handle_turn) liittää tämän muistoon
    /// kirjoitettaessa, ja [`Agent::think`](crate::Agent::think) suodattaa
    /// recallin samalla tagilla — näin eri sessioiden muistot pysyvät erillään
    /// vaikka agentti ja muisti ovat jaettuja (ei per-sessio-instansseja).
    #[must_use]
    pub fn session_tag(&self) -> String {
        format!("{SESSION_TAG_PREFIX}{}", self.session_key())
    }

    /// Reply-kohde tälle alkuperälle: keskustelu, josta viesti tuli.
    ///
    /// F4-reititys käyttää tätä per-viesti **ennen** staattista reply-kohdetta:
    /// vastaus ohjautuu takaisin samaan keskusteluun, ei johonkin kiinteään
    /// kohteeseen. Vastaa kanavakerroksen
    /// [`InboundEnvelope::reply`](familyclaw_channels::InboundEnvelope::reply)
    /// -kohdetta (`conversation`).
    #[must_use]
    pub fn reply_target(&self) -> &str {
        &self.conversation
    }

    /// Kuvaa kanavakerroksen [`InboundEnvelope`](familyclaw_channels::InboundEnvelope):n
    /// alkuperäksi. Tämä on **F2-kytkentäkohta**: kanava tuottaa jo täsmälleen
    /// nämä kentät, joten origin saadaan per-viesti ilman uutta tietoa.
    #[must_use]
    pub fn from_inbound_envelope(envelope: &familyclaw_channels::InboundEnvelope) -> Self {
        Self {
            channel_id: envelope.channel_id.clone(),
            conversation: envelope.conversation.clone(),
            sender: envelope.sender.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_channels::{ChannelKind, InboundMessage};

    #[test]
    fn session_key_is_channel_and_conversation() {
        let origin = MessageOrigin::new("discord-main", "general", "user-42");
        assert_eq!(origin.session_key(), "discord-main:general");
    }

    #[test]
    fn sender_does_not_affect_session_key() {
        // Sama kanava + keskustelu, eri lähettäjä → SAMA sessio (monenkeskinen).
        let a = MessageOrigin::new("tg-1", "room-7", "alice");
        let b = MessageOrigin::new("tg-1", "room-7", "bob");
        assert_eq!(a.session_key(), b.session_key());
    }

    #[test]
    fn different_conversation_is_different_session() {
        // F4:n ydinväite: eri keskustelu = eri sessio (ei kontekstivuotoa).
        let a = MessageOrigin::new("discord-main", "channel-a", "u");
        let b = MessageOrigin::new("discord-main", "channel-b", "u");
        assert_ne!(a.session_key(), b.session_key());
    }

    #[test]
    fn different_channel_is_different_session() {
        let a = MessageOrigin::new("discord-main", "general", "u");
        let b = MessageOrigin::new("tg-main", "general", "u");
        assert_ne!(a.session_key(), b.session_key());
    }

    #[test]
    fn session_tag_prefixes_session_key() {
        let origin = MessageOrigin::new("discord-main", "general", "u");
        assert_eq!(origin.session_tag(), "session:discord-main:general");
        assert!(origin.session_tag().starts_with(SESSION_TAG_PREFIX));
        // Tag sisältää koko session-avaimen.
        assert!(origin.session_tag().ends_with(&origin.session_key()));
    }

    #[test]
    fn reply_target_is_conversation() {
        let origin = MessageOrigin::new("discord-main", "general", "u");
        assert_eq!(origin.reply_target(), "general");
    }

    #[test]
    fn from_inbound_envelope_maps_origin_fields() {
        // F2-kytkentäkohta: kanavakirjekuori → MessageOrigin (per-viesti).
        let envelope = InboundMessage::new("user-42", "general", "hei")
            .expect("valid inbound")
            .into_envelope(ChannelKind::Discord, "discord-main");
        let origin = MessageOrigin::from_inbound_envelope(&envelope);
        assert_eq!(origin.channel_id, "discord-main");
        assert_eq!(origin.conversation, "general");
        assert_eq!(origin.sender, "user-42");
        // Session-avain johdettu suoraan kirjekuoresta.
        assert_eq!(origin.session_key(), "discord-main:general");
        // Reply-kohde = sama keskustelu kuin kirjekuoren reply().
        assert_eq!(origin.reply_target(), envelope.conversation);
    }

    #[test]
    fn message_origin_serde_roundtrip() {
        // Serde-sarjallistuva: valmis bus-kirjekuoren origin-kenttään (F2) ja
        // durable-replayhin.
        let origin = MessageOrigin::new("discord-main", "general", "user-42");
        let json = serde_json::to_string(&origin).expect("serialize");
        let back: MessageOrigin = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(origin, back);
    }
}
