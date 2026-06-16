//! `μACP` — **micro Agent Communication Protocol**: nelivervinen
//! viestintäkalkyyli ([`AcpVerb`]) Resonance Busin päälle.
//!
//! Tausta (design §2, `SOLID_PLAN`): agenttien välinen viestintä
//! pelkistyy neljään perfomatiiviin — `PING` (liveness), `TELL` (fakta),
//! `ASK` (kysely joka odottaa vastausta) ja `OBSERVE` (tapahtuma). Tämä on
//! sama ydin kuin klassisessa FIPA-ACL:ssä, mutta riisuttu minimiin matalan
//! latenssin (≈34 ms) viestintää varten.
//!
//! ## Suhde olemassa olevaan busiin
//! Tämä moduuli **ei korvaa** [`BusHandle::publish`]-polkua — se *kääntää*
//! verbin olemassa olevaksi [`BusMessage`]:ksi ja julkaisee sen normaalia
//! reittiä ([`BusHandle::send_acp`]). Verbi ja vapaaehtoinen kohde
//! ([`AcpEnvelope::to`]) kulkevat mukana metatietona, jotta vastaanottaja voi
//! reitittää ja suodattaa viestit performatiivin mukaan. Bus itse pysyy
//! broadcast-mallisena (toimitus kaikille muille) — kohde on *aiottu*
//! vastaanottaja, jonka muut olennot voivat sivuuttaa.
//!
//! ## OSS-raja (KERROS A)
//! Ei kovakoodattuja perheen nimiä, ID:itä eikä avaimia. Olentotunnisteet ja
//! sisältö annetaan ajonaikaisesti; esimerkit käyttävät geneerisiä nimiä
//! (`agent_a`).

use serde::{Deserialize, Serialize};

use familyclaw_core::Result;

use crate::bus::BusHandle;
use crate::message::{BeingId, BusMessage};

/// Vakaa `name`-tunniste μACP-viesteille [`BusMessage::Custom`]-kuoressa.
///
/// Vastaanottaja erottaa μACP-liikenteen muusta busiliikenteestä tämän
/// nimen perusteella; verbi ja kohde löytyvät hyötykuorman JSON-kentistä.
pub const ACP_MESSAGE_NAME: &str = "uacp";

/// μACP:n neljä performatiivia (puheaktia).
///
/// Tarkoituksella minimaalinen joukko — laajempi semantiikka rakennetaan
/// näiden päälle ylemmissä kerroksissa, ei lisäämällä variantteja tähän.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpVerb {
    /// **Liveness**-koetus: "oletko elossa?" Ei odota sisältövastausta, vain
    /// merkki olennon tavoitettavuudesta.
    Ping,
    /// **Fakta**: lähettäjä kertoo jotain todeksi uskomaansa. Ei odota vastausta.
    Tell,
    /// **Kysely**: lähettäjä pyytää tietoa ja **odottaa vastausta** (yleensä
    /// vastaus-`Tell` takaisin).
    Ask,
    /// **Tapahtuma**: lähettäjä julkaisee havainnon/tapahtuman, jonka muut
    /// voivat huomioida. Tilatieto, ei suora pyyntö.
    Observe,
}

impl AcpVerb {
    /// Lyhyt vakaa tunniste lokitusta, reititystä ja metriikkaa varten.
    #[must_use]
    pub const fn as_label(&self) -> &'static str {
        match self {
            AcpVerb::Ping => "ping",
            AcpVerb::Tell => "tell",
            AcpVerb::Ask => "ask",
            AcpVerb::Observe => "observe",
        }
    }

    /// Odottaako tämä performatiivi vastausta? (Vain [`Ask`](AcpVerb::Ask).)
    #[must_use]
    pub const fn expects_reply(&self) -> bool {
        matches!(self, AcpVerb::Ask)
    }
}

/// μACP-kirjekuori: performatiivi + lähettäjä + (valinnainen) kohde + sisältö.
///
/// `to` on **aiottu** vastaanottaja. Resonance Bus toimittaa broadcastina
/// kaikille muille olennoille, joten kohde on suodatusvihje vastaanottajalle,
/// ei kova reititysrajoite. `None` tarkoittaa kaikille suunnattua viestiä.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpEnvelope {
    /// Puheakti (performatiivi).
    pub verb: AcpVerb,
    /// Viestin lähettävä olento.
    pub from: BeingId,
    /// Aiottu vastaanottaja, tai `None` jos viesti on suunnattu kaikille.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<BeingId>,
    /// Vapaamuotoinen tekstihyötykuorma (totuuden lähde, kuten busin muuallakin).
    pub payload: String,
}

impl AcpEnvelope {
    /// Rakentaa kaikille suunnatun kirjekuoren (`to = None`).
    pub fn broadcast(verb: AcpVerb, from: BeingId, payload: impl Into<String>) -> Self {
        Self {
            verb,
            from,
            to: None,
            payload: payload.into(),
        }
    }

    /// Rakentaa yhdelle olennolle suunnatun kirjekuoren.
    pub fn directed(verb: AcpVerb, from: BeingId, to: BeingId, payload: impl Into<String>) -> Self {
        Self {
            verb,
            from,
            to: Some(to),
            payload: payload.into(),
        }
    }

    /// Apuri: `PING`-kirjekuori liveness-koetukseen (kaikille).
    pub fn ping(from: BeingId) -> Self {
        Self::broadcast(AcpVerb::Ping, from, String::new())
    }

    /// Apuri: `TELL`-kirjekuori (fakta) kaikille.
    pub fn tell(from: BeingId, payload: impl Into<String>) -> Self {
        Self::broadcast(AcpVerb::Tell, from, payload)
    }

    /// Apuri: `ASK`-kirjekuori (kysely) tietylle olennolle.
    pub fn ask(from: BeingId, to: BeingId, payload: impl Into<String>) -> Self {
        Self::directed(AcpVerb::Ask, from, to, payload)
    }

    /// Apuri: `OBSERVE`-kirjekuori (tapahtuma) kaikille.
    pub fn observe(from: BeingId, payload: impl Into<String>) -> Self {
        Self::broadcast(AcpVerb::Observe, from, payload)
    }

    /// Kääntää μACP-kirjekuoren busin omaksi [`BusMessage`]:ksi
    /// ([`BusMessage::Custom`], nimi [`ACP_MESSAGE_NAME`]). Verbi, kohde ja
    /// sisältö koodataan JSON-hyötykuormaan, jotta vastaanottaja voi tulkita
    /// performatiivin ja palauttaa kirjekuoren ([`AcpEnvelope::from_bus_message`]).
    #[must_use]
    pub fn to_bus_message(&self) -> BusMessage {
        BusMessage::Custom {
            name: ACP_MESSAGE_NAME.to_string(),
            payload: serde_json::json!({
                "verb": self.verb,
                "to": self.to,
                "payload": self.payload,
            }),
        }
    }

    /// Kuten [`to_bus_message`](Self::to_bus_message), mutta **kuluttaa**
    /// kirjekuoren (välttää tekstisisällön kloonauksen lähetyspolulla).
    #[must_use]
    pub fn into_bus_message(self) -> BusMessage {
        BusMessage::Custom {
            name: ACP_MESSAGE_NAME.to_string(),
            payload: serde_json::json!({
                "verb": self.verb,
                "to": self.to,
                "payload": self.payload,
            }),
        }
    }

    /// Yrittää lukea μACP-kirjekuoren takaisin busiviestistä. Palauttaa `None`
    /// jos viesti ei ole μACP-viesti (väärä nimi) tai hyötykuorma ei jäsenny.
    ///
    /// `from` ei kulje [`BusMessage`]:ssä (se on kirjekuoren
    /// [`ResonanceMessage::from`]-kentässä), joten se annetaan erikseen.
    ///
    /// [`ResonanceMessage::from`]: crate::message::ResonanceMessage::from
    #[must_use]
    pub fn from_bus_message(from: BeingId, msg: &BusMessage) -> Option<Self> {
        let BusMessage::Custom { name, payload } = msg else {
            return None;
        };
        if name != ACP_MESSAGE_NAME {
            return None;
        }
        let verb: AcpVerb = serde_json::from_value(payload.get("verb")?.clone()).ok()?;
        let to: Option<BeingId> = match payload.get("to") {
            Some(serde_json::Value::Null) | None => None,
            Some(v) => serde_json::from_value(v.clone()).ok()?,
        };
        let body = payload.get("payload")?.as_str()?.to_string();
        Some(Self {
            verb,
            from,
            to,
            payload: body,
        })
    }
}

impl BusHandle {
    /// Lähettää μACP-kirjekuoren olemassa olevaa [`publish`](BusHandle::publish)-
    /// polkua pitkin. Verbi käännetään [`BusMessage::Custom`]:ksi
    /// ([`AcpEnvelope::to_bus_message`]); itse julkaisu, broadcast ja
    /// supervision toimivat täsmälleen kuten tavallisella publishilla.
    ///
    /// Tämä on **lisäys** publishin päälle, ei korvaus: kaikki olemassa oleva
    /// busiliikenne jatkaa entiseen tapaan.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] jos viestin lähetys busille epäonnistuu.
    ///
    /// [`FamilyClawError::Bus`]: familyclaw_core::FamilyClawError::Bus
    pub fn send_acp(&self, envelope: AcpEnvelope) -> Result<()> {
        let from = envelope.from;
        self.publish(from, envelope.into_bus_message())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::being::{BeingInfo, CollectedLog, CollectorBeing};
    use crate::message::ResonanceMessage;
    use crate::ResonanceBus;
    use ractor::{Actor, ActorRef};
    use std::time::Duration as StdDuration;

    async fn join_being(
        bus: &BusHandle,
        name: &str,
    ) -> (BeingId, ActorRef<ResonanceMessage>, CollectedLog) {
        let log = CollectorBeing::new_log();
        let (actor, _h) = Actor::spawn(None, CollectorBeing, log.clone())
            .await
            .expect("spawn being");
        let id = BeingId::new();
        bus.register(BeingInfo::new(id, name, actor.clone()))
            .expect("register");
        (id, actor, log)
    }

    async fn settle() {
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    }

    fn log_len(log: &CollectedLog) -> usize {
        log.lock().expect("lock").len()
    }

    #[test]
    fn verb_labels_and_reply_semantics() {
        assert_eq!(AcpVerb::Ping.as_label(), "ping");
        assert_eq!(AcpVerb::Tell.as_label(), "tell");
        assert_eq!(AcpVerb::Ask.as_label(), "ask");
        assert_eq!(AcpVerb::Observe.as_label(), "observe");

        // Vain ASK odottaa vastausta.
        assert!(AcpVerb::Ask.expects_reply());
        assert!(!AcpVerb::Ping.expects_reply());
        assert!(!AcpVerb::Tell.expects_reply());
        assert!(!AcpVerb::Observe.expects_reply());
    }

    #[test]
    fn envelope_roundtrips_through_bus_message_for_all_verbs() {
        let from = BeingId::new();
        let to = BeingId::new();
        let cases = [
            AcpEnvelope::ping(from),
            AcpEnvelope::tell(from, "taivas on sininen"),
            AcpEnvelope::ask(from, to, "mikä kello on?"),
            AcpEnvelope::observe(from, "ovi avautui"),
        ];

        for env in cases {
            let msg = env.to_bus_message();
            // Kääntyy Custom-viestiksi vakaalla nimellä.
            match &msg {
                BusMessage::Custom { name, .. } => assert_eq!(name, ACP_MESSAGE_NAME),
                other => panic!("odotettiin Custom, saatiin {other:?}"),
            }
            // Ja palautuu takaisin samaksi kirjekuoreksi.
            let back = AcpEnvelope::from_bus_message(env.from, &msg)
                .expect("μACP-viesti jäsentyy takaisin");
            assert_eq!(back, env, "verbi {} ei roundtripannut", env.verb.as_label());
        }
    }

    #[test]
    fn non_acp_custom_message_is_not_parsed() {
        let msg = BusMessage::Custom {
            name: "ei-uacp".to_string(),
            payload: serde_json::json!({ "verb": "ping" }),
        };
        assert!(AcpEnvelope::from_bus_message(BeingId::new(), &msg).is_none());

        // Myös ei-Custom-viesti palauttaa None.
        assert!(AcpEnvelope::from_bus_message(BeingId::new(), &BusMessage::text("hei")).is_none());
    }

    #[test]
    fn directed_vs_broadcast_target() {
        let from = BeingId::new();
        let to = BeingId::new();
        assert_eq!(AcpEnvelope::tell(from, "x").to, None);
        assert_eq!(AcpEnvelope::ask(from, to, "y").to, Some(to));
        assert_eq!(AcpEnvelope::ping(from).to, None);
        assert_eq!(AcpEnvelope::observe(from, "z").to, None);
    }

    #[test]
    fn envelope_serde_roundtrip() {
        let env = AcpEnvelope::ask(BeingId::new(), BeingId::new(), "payload");
        let json = serde_json::to_string(&env).expect("serialize");
        let back: AcpEnvelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(env, back);
    }

    /// `send_acp` reitittää μACP-viestin OLEMASSA OLEVAN publish-polun yli:
    /// sisarukset saavat sen, lähettäjä ei (sama semantiikka kuin publishilla),
    /// ja viesti jäsentyy oikeaksi verbiksi vastaanottajan päässä.
    #[tokio::test]
    async fn send_acp_routes_verbs_over_publish_path() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, log_a) = join_being(&bus, "agent_a").await;
        let (_id_b, _b, log_b) = join_being(&bus, "agent_b").await;

        bus.send_acp(AcpEnvelope::tell(id_a, "fakta sisaruksille"))
            .expect("send_acp tell");
        settle().await;

        // Lähettäjä ei saa omaa viestiään (publishin broadcast-sääntö pätee).
        assert_eq!(log_len(&log_a), 0, "lähettäjä ei saa omaa μACP-viestiään");
        // Sisarus saa sen, ja se jäsentyy oikeaksi verbiksi.
        assert_eq!(log_len(&log_b), 1, "sisarus saa μACP-viestin");
        let received = log_b.lock().expect("lock")[0].clone();
        assert_eq!(received.from, id_a);
        let acp = AcpEnvelope::from_bus_message(received.from, &received.payload)
            .expect("vastaanotettu viesti on μACP");
        assert_eq!(acp.verb, AcpVerb::Tell);
        assert_eq!(acp.payload, "fakta sisaruksille");
        assert_eq!(acp.from, id_a);

        bus.stop();
    }

    /// Eri verbit reitittyvät kukin omana performatiivinaan saman polun yli.
    #[tokio::test]
    async fn each_verb_arrives_with_correct_performative() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, _la) = join_being(&bus, "agent_a").await;
        let (id_b, _b, log_b) = join_being(&bus, "agent_b").await;

        bus.send_acp(AcpEnvelope::ping(id_a)).expect("ping");
        bus.send_acp(AcpEnvelope::ask(id_a, id_b, "kysymys"))
            .expect("ask");
        bus.send_acp(AcpEnvelope::observe(id_a, "tapahtuma"))
            .expect("observe");
        settle().await;

        let received = log_b.lock().expect("lock");
        assert_eq!(received.len(), 3, "kolme μACP-viestiä toimitettu");
        let verbs: Vec<AcpVerb> = received
            .iter()
            .map(|m| {
                AcpEnvelope::from_bus_message(m.from, &m.payload)
                    .expect("μACP")
                    .verb
            })
            .collect();
        assert_eq!(verbs, vec![AcpVerb::Ping, AcpVerb::Ask, AcpVerb::Observe]);

        // ASK kantoi kohteen (directed), muut eivät.
        let ask = received
            .iter()
            .find_map(|m| {
                let e = AcpEnvelope::from_bus_message(m.from, &m.payload)?;
                (e.verb == AcpVerb::Ask).then_some(e)
            })
            .expect("ask löytyy");
        assert_eq!(ask.to, Some(id_b));

        drop(received);
        bus.stop();
    }
}
