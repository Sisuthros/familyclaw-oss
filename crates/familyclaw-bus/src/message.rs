//! Busin viestityypit: [`BusMessage`] (hyötykuorma) ja [`ResonanceMessage`]
//! (kirjekuori metatietoineen).
//!
//! Resonance Bus on **affektiivinen hermosto** (design §2.2): jokaisen olennon
//! tunnetila voi *vuotaa* busiin, ja muut olennot aistivat sen
//! ([`BusMessage::EmotionPulse`]). Viestit kantavat aina tiedon lähettäjästä
//! ([`ResonanceMessage::from`]), jotta vastaanottaja tietää **kuka** resonoi.
//!
//! ## OSS-raja (KERROS A)
//! Mikään tässä moduulissa ei kovakoodaa perheenjäsenten sieluja, mallinimiä,
//! avaimia eikä polkuja. Olentojen tunnisteet ([`BeingId`]) ja mallitunnisteet
//! annetaan aina ajonaikaisesti; esimerkit käyttävät geneerisiä nimiä
//! (`agent_a`, `agent_b`).

use std::fmt;

use familyclaw_core::ids::{AgentId, MessageId};
use familyclaw_core::time::{self, Timestamp};
use familyclaw_emotion::EmotionState;
use familyclaw_latent::LatentVector;
use serde::{Deserialize, Serialize};

/// Busiin liittyneen olennon (agentin) tunniste.
///
/// Tämä on ohut newtype [`AgentId`]:n ympärillä: bus puhuu *olennoista*
/// (beings) eikä pelkistä agenttitunnisteista, mutta identiteetti on sama.
/// Erillinen tyyppi tekee bus-rajapinnasta itsedokumentoivan ja estää
/// sekoittamasta busin osallistujaa muihin tunnisteisiin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BeingId(AgentId);

impl BeingId {
    /// Luo uuden satunnaisen olennotunnisteen.
    #[must_use]
    pub fn new() -> Self {
        Self(AgentId::new())
    }

    /// Kääri olemassa olevan [`AgentId`]:n olennotunnisteeksi.
    #[must_use]
    pub const fn from_agent_id(id: AgentId) -> Self {
        Self(id)
    }

    /// Palauttaa sisällä olevan [`AgentId`]:n.
    #[must_use]
    pub const fn agent_id(&self) -> AgentId {
        self.0
    }
}

impl Default for BeingId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for BeingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl From<AgentId> for BeingId {
    fn from(id: AgentId) -> Self {
        Self(id)
    }
}

impl From<BeingId> for AgentId {
    fn from(id: BeingId) -> Self {
        id.0
    }
}

/// Tehtäväelinkaaren tapahtumalaji, jonka olento voi julkaista busiin.
///
/// Tämä on tarkoituksella *kevyt ja geneerinen* — varsinainen tehtävämalli
/// elää siltakerroksessa (`familyclaw-bridge`). Bus välittää vain signaalin,
/// jotta sisarukset voivat reagoida toistensa työn etenemiseen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventKind {
    /// Tehtävä luotiin.
    Created,
    /// Tehtävä aloitettiin (siirtyi työn alle).
    Started,
    /// Tehtävän edistymistä päivitettiin.
    Progress,
    /// Tehtävä valmistui onnistuneesti.
    Completed,
    /// Tehtävä epäonnistui.
    Failed,
    /// Tehtävä luovutettiin toiselle olennolle.
    HandedOff,
    /// Sovelluskohtainen tapahtumalaji vapaalla nimellä.
    Custom(String),
}

impl TaskEventKind {
    /// Palauttaa lajin vakaan tunnisteen merkkijonona (lokitukseen, reititykseen).
    #[must_use]
    pub fn as_label(&self) -> &str {
        match self {
            TaskEventKind::Created => "created",
            TaskEventKind::Started => "started",
            TaskEventKind::Progress => "progress",
            TaskEventKind::Completed => "completed",
            TaskEventKind::Failed => "failed",
            TaskEventKind::HandedOff => "handed_off",
            TaskEventKind::Custom(name) => name.as_str(),
        }
    }
}

/// Busissa kulkevan viestin hyötykuorma.
///
/// Tämä on Resonance Busin "kieli". Variantit kattavat sekä tavanomaisen
/// teksti-/tehtäväviestinnän että affektiivisen hermoston ydinviestit
/// ([`EmotionPulse`](BusMessage::EmotionPulse), [`Latent`](BusMessage::Latent)).
///
/// `#[non_exhaustive]` jotta uusia viestityyppejä voi lisätä rikkomatta
/// downstream-koodia.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BusMessage {
    /// Tavallinen tekstiviesti olentojen välillä.
    Text {
        /// Viestin tekstisisältö.
        body: String,
    },

    /// Latent-telepatia: olennon piilotila ([`LatentVector`]) sekä aina mukana
    /// kulkeva tekstivarjo. Teksti on totuuden lähde — latent on optimointi
    /// (design §2.4, ks. `familyclaw-latent`).
    Latent {
        /// Lähettävän mallin hidden-state-vektori.
        vector: LatentVector,
        /// Tekstivarjo, johon vastaanottaja palaa jos latent ei sovellu.
        text_shadow: String,
    },

    /// **Affektiivinen pulssi:** olennon tunnetila vuotaa busiin. Tämä on
    /// affective contagion -mekanismin perusviesti — kun yksi sisarus on
    /// esim. luovassa virtauksessa, muut aistivat sen.
    EmotionPulse {
        /// Lähettävän olennon hetkellinen tunnetila.
        state: EmotionState,
    },

    /// Tehtäväelinkaaren tapahtuma (kevyt signaali; täysi malli siltakerroksessa).
    TaskEvent {
        /// Tapahtuman laji.
        event: TaskEventKind,
        /// Tehtävän tunniste (vapaamuotoinen, esim. siltakerroksen id).
        task_id: String,
        /// Vapaaehtoinen ihmisluettava kuvaus.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },

    /// Sovellus-/adapterikohtainen viesti vapaalla JSON-hyötykuormalla.
    /// Mahdollistaa laajennukset ilman ydintyypin muuttamista.
    Custom {
        /// Viestin laji (vapaa nimi).
        name: String,
        /// JSON-hyötykuorma.
        payload: serde_json::Value,
    },
}

impl BusMessage {
    /// Rakentaa tekstiviestin.
    pub fn text(body: impl Into<String>) -> Self {
        BusMessage::Text { body: body.into() }
    }

    /// Rakentaa affektiivisen pulssin annetusta tunnetilasta.
    #[must_use]
    pub fn emotion_pulse(state: EmotionState) -> Self {
        BusMessage::EmotionPulse { state }
    }

    /// Rakentaa latent-viestin piilotilasta ja tekstivarjosta.
    pub fn latent(vector: LatentVector, text_shadow: impl Into<String>) -> Self {
        BusMessage::Latent {
            vector,
            text_shadow: text_shadow.into(),
        }
    }

    /// Rakentaa tehtävätapahtuman.
    pub fn task_event(event: TaskEventKind, task_id: impl Into<String>) -> Self {
        BusMessage::TaskEvent {
            event,
            task_id: task_id.into(),
            detail: None,
        }
    }

    /// Onko tämä affektiivinen pulssi (contagion-reititys nojaa tähän).
    #[must_use]
    pub fn is_emotion_pulse(&self) -> bool {
        matches!(self, BusMessage::EmotionPulse { .. })
    }

    /// Lyhyt lajitunniste lokitusta ja metriikkaa varten.
    #[must_use]
    pub fn kind_label(&self) -> &'static str {
        match self {
            BusMessage::Text { .. } => "text",
            BusMessage::Latent { .. } => "latent",
            BusMessage::EmotionPulse { .. } => "emotion_pulse",
            BusMessage::TaskEvent { .. } => "task_event",
            BusMessage::Custom { .. } => "custom",
        }
    }
}

/// Busin läpi kulkeva viesti **kirjekuoressa**: hyötykuorma + lähettäjä +
/// tunniste + aikaleima.
///
/// Bus rikastaa jokaisen julkaisun tähän muotoon, jotta vastaanottajat
/// tietävät kuka resonoi ja milloin. Kuori on `Clone`, koska sama viesti
/// monistetaan jokaiselle vastaanottavalle olennolle (broadcast).
/// Yhden saapuvan viestin **alkuperae** bus-kirjekuoressa (F2-origin-sopimus):
/// mistae kanavasta, mistae keskustelusta ja keneltae viesti tuli.
///
/// Tama on **kevyt, riippumaton** tyyppi busin matalimmassa kerroksessa: vain
/// kolme [`String`]-kenttaa, jotta bus ei joudu riippumaan kanava- tai
/// agenttikerroksesta (kerrossuunta on yksisuuntainen — ne riippuvat busista,
/// ei toisin pain). Agenttikerroksen `MessageOrigin` kuvautuu suoraan tahan ja
/// takaisin (`channel_id`/`conversation`/`sender` tasmaavat kenttae kentaelta).
///
/// ## Miksi origin kuuluu kirjekuoreen, ei hyotykuormaan
/// [`BusMessage`] on *sisaeltoe* (teksti/tunnepulssi/latent). Alkuperae on
/// *metatietoa* siitae mistae sisaeltoe saapui — sama tieto kuin laehettaejae
/// ([`ResonanceMessage::from`]) ja aikaleima. Origin kirjekuoressa antaa
/// vastaanottajalle (agentille) keinon johtaa **vastauksen kohde** per viesti
/// (`conversation`), niin etta useampi keskustelu ei reitity vaerin.
///
/// ## OSS-raja (KERROS A)
/// Ei kovakoodattuja kanavanimiae, keskusteluja eikae avaimia — kaikki tulee
/// ajonaikaisesti saapuvasta viestistae.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageOrigin {
    /// Kanavainstanssin tunniste (esim. `"discord-main"`).
    pub channel_id: String,
    /// Keskustelun/ryhmaen tunniste (vastausosoite).
    pub conversation: String,
    /// Kanavakohtainen laehettaejaen tunniste (auditointi).
    pub sender: String,
}

impl MessageOrigin {
    /// Rakentaa alkuperaen paljaista osista.
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

    /// Reply-kohde taelle alkuperaelle: keskustelu, josta viesti tuli.
    #[must_use]
    pub fn reply_target(&self) -> &str {
        &self.conversation
    }
}

/// Busin läpi kulkeva viesti **kirjekuoressa**: hyötykuorma + lähettäjä +
/// tunniste + aikaleima + valinnainen per-viesti-alkuperä ([`origin`]).
///
/// Bus rikastaa jokaisen julkaisun tähän muotoon, jotta vastaanottajat
/// tietävät kuka resonoi ja milloin. Kuori on `Clone`, koska sama viesti
/// monistetaan jokaiselle vastaanottavalle olennolle (broadcast).
///
/// [`origin`] (F2-origin-sopimus) kantaa ulkomaailman alkuperän
/// ([`MessageOrigin`]: kanava + keskustelu + lähettäjä) silloin kun viesti tuli
/// kanavakerroksen kautta. Vastaanottava agentti johtaa siitä vastauksen
/// kohteen per viesti; `None`-tapauksessa palataan staattiseen reply-kohteeseen.
///
/// [`origin`]: ResonanceMessage::origin
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResonanceMessage {
    /// Viestin yksilöivä tunniste.
    pub id: MessageId,
    /// Viestin lähettänyt olento.
    pub from: BeingId,
    /// Lähetyshetken UTC-aikaleima.
    pub at: Timestamp,
    /// Varsinainen hyötykuorma.
    pub payload: BusMessage,
    /// Per-viesti-alkuperä (kanava/keskustelu/lähettäjä), jos viesti tuli
    /// ulkomaailmasta kanavakerroksen kautta. Sisäisesti syntyneet viestit
    /// (olentojen väliset pulssit, say/announce) jättävät sen `None`:ksi.
    /// `#[serde(default)]` → vanhat (origin-ttömät) durable-replayt
    /// deserialisoituvat `None`:ksi (taaksepäin-yhteensopiva).
    #[serde(default)]
    pub origin: Option<MessageOrigin>,
}

impl ResonanceMessage {
    /// Rakentaa kirjekuoren tuoreella tunnisteella ja nykyhetken aikaleimalla.
    /// Origin on `None` (sisäinen viesti); ulkomaailman alkuperä liitetään
    /// [`with_origin`](Self::with_origin):llä.
    #[must_use]
    pub fn new(from: BeingId, payload: BusMessage) -> Self {
        Self {
            id: MessageId::new(),
            from,
            at: time::now(),
            payload,
            origin: None,
        }
    }

    /// Liittää per-viesti-alkuperän kirjekuoreen (F2). Palauttaa `self`
    /// ketjutusta varten.
    #[must_use]
    pub fn with_origin(mut self, origin: MessageOrigin) -> Self {
        self.origin = Some(origin);
        self
    }

    /// Per-viesti-alkuperä, jos asetettu.
    #[must_use]
    pub fn origin(&self) -> Option<&MessageOrigin> {
        self.origin.as_ref()
    }

    /// Onko tämän kirjekuoren hyötykuorma affektiivinen pulssi.
    #[must_use]
    pub fn is_emotion_pulse(&self) -> bool {
        self.payload.is_emotion_pulse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_emotion::Dimension;

    #[test]
    fn being_id_wraps_agent_id_transparently() {
        let agent = AgentId::new();
        let being = BeingId::from_agent_id(agent);
        assert_eq!(being.agent_id(), agent);

        // serde on transparentti: olennotunniste sarjallistuu kuin agentti.
        let being_json = serde_json::to_string(&being).expect("ser being");
        let agent_json = serde_json::to_string(&agent).expect("ser agent");
        assert_eq!(being_json, agent_json);

        let back: BeingId = serde_json::from_str(&being_json).expect("de being");
        assert_eq!(back, being);
    }

    #[test]
    fn being_id_conversions_roundtrip() {
        let agent = AgentId::new();
        let being: BeingId = agent.into();
        let back: AgentId = being.into();
        assert_eq!(agent, back);
    }

    #[test]
    fn being_id_new_and_default_are_unique() {
        assert_ne!(BeingId::new(), BeingId::new());
        assert_ne!(BeingId::default(), BeingId::default());
    }

    #[test]
    fn task_event_kind_labels() {
        assert_eq!(TaskEventKind::Created.as_label(), "created");
        assert_eq!(TaskEventKind::Completed.as_label(), "completed");
        assert_eq!(TaskEventKind::Custom("deploy".into()).as_label(), "deploy");
    }

    #[test]
    fn bus_message_constructors_and_labels() {
        assert_eq!(BusMessage::text("hi").kind_label(), "text");

        let pulse = BusMessage::emotion_pulse(EmotionState::neutral());
        assert!(pulse.is_emotion_pulse());
        assert_eq!(pulse.kind_label(), "emotion_pulse");

        let latent = BusMessage::latent(LatentVector::new(vec![0.1], "agent_a/v1"), "shadow");
        assert_eq!(latent.kind_label(), "latent");
        assert!(!latent.is_emotion_pulse());

        let task = BusMessage::task_event(TaskEventKind::Started, "task-1");
        assert_eq!(task.kind_label(), "task_event");
    }

    #[test]
    fn bus_message_serde_roundtrip_all_variants() {
        let mut state = EmotionState::neutral();
        state.stimulate(Dimension::Joy, 50.0);

        let messages = vec![
            BusMessage::text("hello"),
            BusMessage::emotion_pulse(state),
            BusMessage::latent(LatentVector::new(vec![1.0, 2.0], "agent_a/v1"), "shadow"),
            BusMessage::task_event(TaskEventKind::Progress, "task-9"),
            BusMessage::Custom {
                name: "ping".into(),
                payload: serde_json::json!({ "n": 1 }),
            },
        ];

        for msg in messages {
            let json = serde_json::to_string(&msg).expect("serialize");
            let back: BusMessage = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(msg, back);
        }
    }

    #[test]
    fn resonance_message_carries_sender_and_timestamp() {
        let from = BeingId::new();
        let before = time::now();
        let envelope = ResonanceMessage::new(from, BusMessage::text("hi"));
        let after = time::now();

        assert_eq!(envelope.from, from);
        assert!(envelope.at >= before && envelope.at <= after);
        assert!(!envelope.id.is_nil());
        assert!(!envelope.is_emotion_pulse());
    }

    #[test]
    fn resonance_message_detects_emotion_pulse() {
        let env = ResonanceMessage::new(
            BeingId::new(),
            BusMessage::emotion_pulse(EmotionState::neutral()),
        );
        assert!(env.is_emotion_pulse());
    }

    #[test]
    fn resonance_message_serde_roundtrip() {
        let env = ResonanceMessage::new(BeingId::new(), BusMessage::text("roundtrip"));
        let json = serde_json::to_string(&env).expect("serialize");
        let back: ResonanceMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(env, back);
    }

    // ---- F2 per-viesti-alkuperä (origin) -------------------------------

    #[test]
    fn new_message_has_no_origin_by_default() {
        // Sisäisesti syntyneet viestit (say/pulse) eivät kanna alkuperää.
        let env = ResonanceMessage::new(BeingId::new(), BusMessage::text("sisäinen"));
        assert!(env.origin().is_none());
    }

    #[test]
    fn with_origin_attaches_per_message_origin() {
        let origin = MessageOrigin::new("discord-main", "general", "user-42");
        let env = ResonanceMessage::new(BeingId::new(), BusMessage::text("ulkoa"))
            .with_origin(origin.clone());
        assert_eq!(env.origin(), Some(&origin));
        assert_eq!(env.origin().expect("origin").reply_target(), "general");
    }

    #[test]
    fn message_origin_serde_roundtrip() {
        let origin = MessageOrigin::new("tg-1", "room-7", "alice");
        let json = serde_json::to_string(&origin).expect("serialize");
        let back: MessageOrigin = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(origin, back);
    }

    #[test]
    fn resonance_message_with_origin_serde_roundtrip() {
        let env = ResonanceMessage::new(BeingId::new(), BusMessage::text("origin-roundtrip"))
            .with_origin(MessageOrigin::new("discord-main", "channel-a", "u"));
        let json = serde_json::to_string(&env).expect("serialize");
        let back: ResonanceMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(env, back);
        assert_eq!(back.origin().expect("origin").conversation, "channel-a");
    }

    #[test]
    fn legacy_envelope_without_origin_field_deserializes_to_none() {
        // Taaksepäin-yhteensopivuus: vanha durable-replay-rivi EI sisällä
        // `origin`-kenttää. `#[serde(default)]` → None, ei deserialisointivirhe.
        // Rakennetaan "vanha" muoto serialisoimalla nykyinen ja poistamalla
        // origin-avain JSON-objektista (faithfulimpi kuin käsin koodattu rivi:
        // id/timestamp-muodot pysyvät oikeina riippumatta sisäisestä esityksestä).
        let env = ResonanceMessage::new(BeingId::new(), BusMessage::text("vanha"));
        let mut value: serde_json::Value = serde_json::to_value(&env).expect("to value");
        value
            .as_object_mut()
            .expect("object")
            .remove("origin")
            .expect("nykyinen muoto sisältää origin-avaimen");
        assert!(
            value.get("origin").is_none(),
            "origin-avain poistettu (simuloi vanhaa riviä)"
        );

        let back: ResonanceMessage =
            serde_json::from_value(value).expect("legacy envelope deserializes");
        assert!(back.origin().is_none(), "puuttuva origin-kenttä → None");
        match back.payload {
            BusMessage::Text { body } => assert_eq!(body, "vanha"),
            other => panic!("expected Text, got {other:?}"),
        }
    }
}
