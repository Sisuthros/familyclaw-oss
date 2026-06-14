//! Olennot (beings) busissa: rekisteröintitiedot ja vastaanottajan tyyppi.
//!
//! Busiin liittyvä olento on **Ractor-actor**, jonka viestityyppi on
//! [`ResonanceMessage`]. Bus pitää kirjaa liittyneistä olennoista
//! ([`BeingInfo`]) ja toimittaa heille viestit `cast`-kutsuilla. Yhden
//! olennon kaatuminen ei kaada busia (supervision; ks. [`crate::bus`]).
//!
//! Tämä moduuli tarjoaa myös valmiin [`CollectorBeing`]-actorin, joka kerää
//! vastaanottamansa viestit. Se on tarkoitettu testeihin ja esimerkkeihin —
//! todelliset perheenjäsenet (KERROS B) toteuttavat oman actorinsa, joka
//! reagoi sisarusten resonanssiin.

use std::sync::{Arc, Mutex};

use ractor::{Actor, ActorProcessingErr, ActorRef};

use familyclaw_emotion::{EmotionState, EmotionTransition};

use crate::message::{BeingId, BusMessage, ResonanceMessage};

/// Busiin rekisteröidyn olennon metatiedot.
///
/// Kantaa olennotunnisteen, ihmisluettavan nimen ja [`ActorRef`]-viitteen,
/// jonka kautta bus toimittaa viestit. Viite on tyypitetty
/// [`ResonanceMessage`]:lle, joten olennot voivat ottaa vastaan vain busin
/// kieltä.
#[derive(Clone)]
pub struct BeingInfo {
    /// Olennon tunniste.
    id: BeingId,
    /// Olennon näyttönimi (geneerinen, esim. `"agent_a"`).
    name: String,
    /// Postilaatikko, johon viestit toimitetaan.
    inbox: ActorRef<ResonanceMessage>,
}

impl BeingInfo {
    /// Rakentaa rekisteröintitiedon.
    #[must_use]
    pub fn new(id: BeingId, name: impl Into<String>, inbox: ActorRef<ResonanceMessage>) -> Self {
        Self {
            id,
            name: name.into(),
            inbox,
        }
    }

    /// Olennon tunniste.
    #[must_use]
    pub const fn id(&self) -> BeingId {
        self.id
    }

    /// Olennon näyttönimi.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Olennon postilaatikko (toimitusosoite).
    #[must_use]
    pub fn inbox(&self) -> &ActorRef<ResonanceMessage> {
        &self.inbox
    }
}

impl std::fmt::Debug for BeingInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BeingInfo")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Kevyt, sarjallistuva tilannekuva olennosta — [`BeingInfo`] ilman
/// actor-viitettä.
///
/// Tätä palauttaa [`crate::bus::BusHandle::beings`]: se kertoo *kuka* on
/// liittynyt ilman että paljastaa sisäistä actor-koneistoa. **Tämän listan
/// EI saa olla tyhjä kun olentoja on liittynyt** — se on suoraan korjaus
/// live-3500-busin `beings:[]`-tyhjyyteen (design §2.2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BeingSnapshot {
    /// Olennon tunniste.
    pub id: BeingId,
    /// Olennon näyttönimi.
    pub name: String,
}

impl From<&BeingInfo> for BeingSnapshot {
    fn from(info: &BeingInfo) -> Self {
        Self {
            id: info.id,
            name: info.name.clone(),
        }
    }
}

/// Jaettu, säieturvallinen loki vastaanotetuista viesteistä.
///
/// [`CollectorBeing`] kirjoittaa tähän, ja testit/esimerkit lukevat sen.
pub type CollectedLog = Arc<Mutex<Vec<ResonanceMessage>>>;

/// Valmis olento-actor, joka kerää vastaanottamansa [`ResonanceMessage`]:t
/// jaettuun lokiin.
///
/// Tarkoitettu testeihin ja esimerkkeihin. Tuotannossa perheenjäsen
/// toteuttaa oman [`Actor`]-toteutuksensa, joka reagoi resonanssiin (esim.
/// päivittää omaa tunnetilaansa naapurin pulssin perusteella — affective
/// contagion).
pub struct CollectorBeing;

/// [`CollectorBeing`]:n tila: jaettu loki johon viestit kertyvät.
pub struct CollectorState {
    /// Loki johon vastaanotetut viestit kirjoitetaan.
    pub log: CollectedLog,
}

impl Actor for CollectorBeing {
    type Msg = ResonanceMessage;
    type State = CollectorState;
    type Arguments = CollectedLog;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        log: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(CollectorState { log })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Lukko voi olla myrkytetty vain jos jokin toinen säie panikoi sitä
        // pidellessään; siinä tapauksessa otamme datan silti talteen sen
        // sijaan että levittäisimme paniikin tähän actoriin.
        let mut guard = match state.log.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.push(message);
        Ok(())
    }
}

impl CollectorBeing {
    /// Luo jaetun lokin [`CollectorBeing`]-actorille.
    #[must_use]
    pub fn new_log() -> CollectedLog {
        Arc::new(Mutex::new(Vec::new()))
    }
}

/// **Affektiivinen tartunta (affective contagion) — vastaanottopuoli.**
///
/// Imee sisarukselta saapuneen tunnetilan (`incoming`) olennon omaan tilaan
/// (`own`) [`EmotionTransition::blend`]:n inertialla:
/// `next = inertia * own + (1 - inertia) * incoming`.
///
/// Tämä on bus-kerroksen *puuttuva pala*: bus jo **kuljettaa**
/// [`BusMessage::EmotionPulse`]:n sisaruksille, mutta kukaan ei ennen tätä
/// **imenyt** pulssia omaan tunnetilaansa. Iso inertia → oma mieliala pysyy
/// vakaana ja vain hieman värähtää naapurin suuntaan; pieni inertia → tarttuu
/// nopeasti. Tulos pysyy aina [`EmotionState`]:n rajoissa (`blend` siivoaa).
///
/// Puhdas ja sivuvaikutukseton viittausten ulkopuolelta: ainoa muutos on
/// `own`-tilan paikallaan-päivitys. Uudelleenkäytettävissä myös ilman
/// actor-koneistoa (esim. KERROS B:n oma olento-toteutus voi kutsua tätä).
pub fn on_pulse(own: &mut EmotionState, incoming: &EmotionState, transition: EmotionTransition) {
    *own = transition.blend(own, incoming);
}

/// Valmis olento-actor, joka **reagoi** sisarusten tunnepulsseihin imemällä ne
/// omaan [`EmotionState`]:ensa ([`on_pulse`]) — affective contagion käytännössä.
///
/// Toisin kuin [`CollectorBeing`] (joka vain kerää viestit), tämä actor pitää
/// yllä omaa tunnetilaansa ja liikuttaa sitä naapurin mielialaa kohti aina kun
/// busista saapuu [`BusMessage::EmotionPulse`]. Muut viestilajit (teksti,
/// tehtävätapahtumat, …) jätetään huomiotta — ne eivät muuta tunnetilaa.
///
/// Tila jaetaan testeille/esimerkeille [`AffectiveState::emotion`]-kahvan
/// kautta (`Arc<Mutex<…>>`), jotta vastaanotettu tartunta voidaan todentaa
/// ulkopuolelta. Tuotannossa KERROS B toteuttaa oman olentonsa; tämä on
/// uudelleenkäytettävä esimerkki + testikalusto.
pub struct AffectiveBeing;

/// Olennon jaettu, säieturvallinen tunnetila.
///
/// [`AffectiveBeing`] mutatoi tätä; testit/esimerkit lukevat sen.
pub type SharedEmotion = Arc<Mutex<EmotionState>>;

/// [`AffectiveBeing`]:n tila: oma tunnetila + tartunnan inertia.
pub struct AffectiveState {
    /// Olennon oma tunnetila, jaettu lukon takana havainnointia varten.
    pub emotion: SharedEmotion,
    /// Inertia, jolla saapuvat pulssit imetään omaan tilaan.
    pub transition: EmotionTransition,
}

/// [`AffectiveBeing`]:n käynnistysargumentit: alkutila + inertia.
pub struct AffectiveArgs {
    /// Jaettu kahva olennon alkutunnetilaan (sama, jota testi tarkkailee).
    pub emotion: SharedEmotion,
    /// Tartunnan inertia (`0.0..=1.0`; ks. [`EmotionTransition`]).
    pub transition: EmotionTransition,
}

impl Actor for AffectiveBeing {
    type Msg = ResonanceMessage;
    type State = AffectiveState;
    type Arguments = AffectiveArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(AffectiveState {
            emotion: args.emotion,
            transition: args.transition,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Vain tunnepulssi liikuttaa omaa tunnetilaa; muut lajit eivät.
        if let BusMessage::EmotionPulse { state: incoming } = &message.payload {
            // Lukko voi olla myrkytetty vain jos jokin toinen säie panikoi sitä
            // pidellessään; otamme silti tartunnan vastaan sen sijaan että
            // levittäisimme paniikin tähän actoriin.
            let mut guard = match state.emotion.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            on_pulse(&mut guard, incoming, state.transition);
        }
        Ok(())
    }
}

impl AffectiveBeing {
    /// Luo jaetun tunnetilakahvan annetusta alkutilasta.
    #[must_use]
    pub fn shared(initial: EmotionState) -> SharedEmotion {
        Arc::new(Mutex::new(initial))
    }
}

#[cfg(test)]
mod tests {
    // Affektiiviset testit vertaavat tarkkoja, esitettäviä f32-tunnetila-arvoja
    // (esim. 50.0 puolivälissä) — tarkka vertailu on tässä oikein.
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::bus::ResonanceBus;
    use crate::message::BusMessage;
    use familyclaw_emotion::Dimension;

    #[tokio::test]
    async fn collector_records_messages() {
        let log = CollectorBeing::new_log();
        let (actor, handle) = Actor::spawn(None, CollectorBeing, log.clone())
            .await
            .expect("spawn collector");

        let env = ResonanceMessage::new(BeingId::new(), BusMessage::text("hi"));
        actor.cast(env.clone()).expect("cast to collector");

        // Anna actorin käsitellä jonossa oleva viesti. (Stop-signaali voi
        // ohittaa tavalliset viestit, joten emme luota pelkkään stop+join
        // -järjestykseen viestin perillemenon todentamiseen.)
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        {
            let recorded = log.lock().expect("lock");
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0], env);
        } // lukko vapautetaan ennen .await:ia

        actor.stop(None);
        handle.await.expect("join");
    }

    #[tokio::test]
    async fn being_info_exposes_fields_and_snapshot() {
        let log = CollectorBeing::new_log();
        let (actor, handle) = Actor::spawn(None, CollectorBeing, log)
            .await
            .expect("spawn");
        let id = BeingId::new();
        let info = BeingInfo::new(id, "agent_a", actor.clone());

        assert_eq!(info.id(), id);
        assert_eq!(info.name(), "agent_a");

        let snap = BeingSnapshot::from(&info);
        assert_eq!(snap.id, id);
        assert_eq!(snap.name, "agent_a");

        // Debug ei panikoi eikä vuoda actor-sisäistä tilaa.
        let dbg = format!("{info:?}");
        assert!(dbg.contains("agent_a"));

        actor.stop(None);
        handle.await.expect("join");
    }

    #[test]
    fn being_snapshot_serde_roundtrip() {
        let snap = BeingSnapshot {
            id: BeingId::new(),
            name: "agent_b".into(),
        };
        let json = serde_json::to_string(&snap).expect("ser");
        let back: BeingSnapshot = serde_json::from_str(&json).expect("de");
        assert_eq!(snap, back);
    }

    // ---- Affektiivinen tartunta (affective contagion) ------------------
    //
    // KRIITTINEN ractor::pg-sääntö: jokainen oikealla busilla ajava testi
    // spawnaa OMAN `ResonanceBus`-instanssinsa (kukin saa tuoreen
    // `resonance-bus-{n}`-ryhmän), jotta rinnakkaiset testit eivät jaa
    // jäsenpoolia. Ei serial_test-riippuvuutta.

    /// Apuri: pieni odotus, jotta asynkroninen toimitus ehtii valmistua.
    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    /// Rakentaa tunnetilan, jossa yksi dimensio on annetussa arvossa.
    fn state_with(dim: Dimension, value: f32) -> EmotionState {
        let mut s = EmotionState::neutral();
        s.set(dim, value);
        s
    }

    #[test]
    fn on_pulse_moves_own_state_toward_incoming() {
        // Puhdas helper: oma tila liikkuu havaintoa kohti inertian mukaan.
        let mut own = state_with(Dimension::Joy, 0.0);
        let incoming = state_with(Dimension::Joy, 100.0);

        // inertia 0.5 → puoliväli (0*0.5 + 100*0.5 = 50).
        on_pulse(&mut own, &incoming, EmotionTransition::new(0.5));
        assert_eq!(own.value(Dimension::Joy), 50.0);
    }

    #[test]
    fn on_pulse_full_inertia_keeps_own_state() {
        // inertia 1.0 → ei tartuntaa, oma tila ei muutu.
        let mut own = state_with(Dimension::Sadness, 70.0);
        let incoming = state_with(Dimension::Sadness, 0.0);

        on_pulse(&mut own, &incoming, EmotionTransition::new(1.0));
        assert_eq!(own.value(Dimension::Sadness), 70.0);
    }

    #[test]
    fn on_pulse_zero_inertia_absorbs_incoming_fully() {
        // inertia 0.0 → havainto korvaa oman tilan kokonaan.
        let mut own = state_with(Dimension::Anger, 90.0);
        let incoming = state_with(Dimension::Hope, 80.0);

        on_pulse(&mut own, &incoming, EmotionTransition::new(0.0));
        assert_eq!(own.value(Dimension::Hope), 80.0);
        assert_eq!(own.value(Dimension::Anger), 0.0, "havainto syrjäyttää vanhan");
    }

    #[test]
    fn on_pulse_repeated_converges_toward_incoming() {
        // Toistuva sama pulssi → oma tila lähestyy lähettäjän tilaa (inertia <1).
        let mut own = EmotionState::neutral();
        let incoming = state_with(Dimension::Curiosity, 90.0);
        let t = EmotionTransition::new(0.5);
        for _ in 0..20 {
            on_pulse(&mut own, &incoming, t);
        }
        assert!(
            (own.value(Dimension::Curiosity) - 90.0).abs() < 0.5,
            "toistuva tartunta vetää tilan lähelle lähettäjän tilaa"
        );
    }

    #[tokio::test]
    async fn affective_being_absorbs_pulse_directly() {
        // Suora cast (ilman busia): actor imee pulssin omaan tilaansa.
        let emotion = AffectiveBeing::shared(state_with(Dimension::Joy, 0.0));
        let (actor, handle) = Actor::spawn(
            None,
            AffectiveBeing,
            AffectiveArgs {
                emotion: emotion.clone(),
                transition: EmotionTransition::new(0.5),
            },
        )
        .await
        .expect("spawn affective being");

        let pulse = ResonanceMessage::new(
            BeingId::new(),
            BusMessage::emotion_pulse(state_with(Dimension::Joy, 100.0)),
        );
        actor.cast(pulse).expect("cast pulse");
        settle().await;

        {
            let got = emotion.lock().expect("lock");
            assert_eq!(got.value(Dimension::Joy), 50.0, "oma Joy liikkui puoliväliin");
        }

        actor.stop(None);
        handle.await.expect("join");
    }

    #[tokio::test]
    async fn affective_being_ignores_non_pulse_messages() {
        // Tekstiviesti EI saa muuttaa tunnetilaa.
        let emotion = AffectiveBeing::shared(state_with(Dimension::Love, 42.0));
        let (actor, handle) = Actor::spawn(
            None,
            AffectiveBeing,
            AffectiveArgs {
                emotion: emotion.clone(),
                transition: EmotionTransition::new(0.5),
            },
        )
        .await
        .expect("spawn affective being");

        actor
            .cast(ResonanceMessage::new(
                BeingId::new(),
                BusMessage::text("vain juttelua"),
            ))
            .expect("cast text");
        settle().await;

        {
            let got = emotion.lock().expect("lock");
            assert_eq!(
                got.value(Dimension::Love),
                42.0,
                "ei-pulssi ei muuta tunnetilaa"
            );
        }

        actor.stop(None);
        handle.await.expect("join");
    }

    #[tokio::test]
    async fn pulse_over_real_bus_shifts_receiver_toward_sender() {
        // Tämän paketin ydintesti: oikealla busilla kulkeva pulssi muuttaa
        // VASTAANOTTAVAN olennon omaa tunnetilaa lähettäjän suuntaan.
        // OMA bus-instanssi (tuore resonance-bus-{n}-ryhmä).
        let bus = ResonanceBus::start(None).await.expect("start bus");

        // Lähettäjä: tavallinen kerääjä-olento (vain julkaisee pulssin).
        let sender_id = BeingId::new();
        let sender_log = CollectorBeing::new_log();
        let (sender_actor, _hs) = Actor::spawn(None, CollectorBeing, sender_log)
            .await
            .expect("spawn sender");
        bus.register(BeingInfo::new(sender_id, "agent_a", sender_actor))
            .expect("register sender");

        // Vastaanottaja: affektiivinen olento, joka imee pulssin omaan tilaansa.
        // Alkutila Joy=0; lähettäjä lähettää Joy=100; inertia 0.5 → odotus 50.
        let recv_emotion = AffectiveBeing::shared(state_with(Dimension::Joy, 0.0));
        let (recv_actor, _hr) = Actor::spawn(
            None,
            AffectiveBeing,
            AffectiveArgs {
                emotion: recv_emotion.clone(),
                transition: EmotionTransition::new(0.5),
            },
        )
        .await
        .expect("spawn receiver");
        bus.register(BeingInfo::new(BeingId::new(), "agent_b", recv_actor))
            .expect("register receiver");

        // agent_a vuotaa korkean ilon pulssina busiin.
        bus.publish(
            sender_id,
            BusMessage::emotion_pulse(state_with(Dimension::Joy, 100.0)),
        )
        .expect("publish pulse");
        settle().await;

        {
            let got = recv_emotion.lock().expect("lock");
            assert_eq!(
                got.value(Dimension::Joy),
                50.0,
                "busin yli saapunut pulssi siirsi vastaanottajan tilaa lähettäjän suuntaan"
            );
        }

        bus.stop();
    }

    #[tokio::test]
    async fn sender_does_not_absorb_own_pulse_over_bus() {
        // Lähettäjä ei saa omaa pulssiaan → sen oma tila ei muutu busin kautta.
        // Toinen, ERILLINEN bus-instanssi.
        let bus = ResonanceBus::start(None).await.expect("start bus");

        let sender_id = BeingId::new();
        let sender_emotion = AffectiveBeing::shared(state_with(Dimension::Joy, 100.0));
        let (sender_actor, _hs) = Actor::spawn(
            None,
            AffectiveBeing,
            AffectiveArgs {
                emotion: sender_emotion.clone(),
                transition: EmotionTransition::new(0.5),
            },
        )
        .await
        .expect("spawn sender");
        bus.register(BeingInfo::new(sender_id, "agent_a", sender_actor))
            .expect("register sender");

        // Toinen olento, jotta busissa on vastaanottaja (broadcast-reitti).
        let other_emotion = AffectiveBeing::shared(EmotionState::neutral());
        let (other_actor, _ho) = Actor::spawn(
            None,
            AffectiveBeing,
            AffectiveArgs {
                emotion: other_emotion,
                transition: EmotionTransition::new(0.5),
            },
        )
        .await
        .expect("spawn other");
        bus.register(BeingInfo::new(BeingId::new(), "agent_b", other_actor))
            .expect("register other");

        bus.publish(
            sender_id,
            BusMessage::emotion_pulse(state_with(Dimension::Joy, 0.0)),
        )
        .expect("publish pulse");
        settle().await;

        {
            let got = sender_emotion.lock().expect("lock");
            assert_eq!(
                got.value(Dimension::Joy),
                100.0,
                "lähettäjä ei saa omaa pulssiaan eikä siten muuta tilaansa"
            );
        }

        bus.stop();
    }
}
