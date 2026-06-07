//! Resonance Bus -actor — affektiivisen hermoston ydin.
//!
//! [`ResonanceBus`] on Ractor-actor, joka:
//! 1. **rekisteröi olennot** ([`BusOp::Register`]) ja poistaa heidät
//!    ([`BusOp::Deregister`]),
//! 2. **lähettää viestit kaikille** muille olennoille ([`BusOp::Publish`]) —
//!    affektiivisen hermoston "veri",
//! 3. **leviää tunnepulssina** (affective contagion): kun olento julkaisee
//!    [`BusMessage::EmotionPulse`]:n, kaikki *muut* olennot saavat sen ja
//!    voivat reagoida toistensa mielialaan,
//! 4. **listaa liittyneet olennot** ([`BusOp::ListBeings`]) — palautettava
//!    lista EI saa olla tyhjä kun olentoja on liittynyt (korjaa live-3500
//!    `beings:[]`, design §2.2),
//! 5. **kestää kaatumiset** (supervision): yksittäisen olennon kuolema ei
//!    kaada busia, vaan poistaa vain kyseisen olennon rekisteristä.
//!
//! Ergonominen rajapinta on [`BusHandle`], joka kääri raa'an
//! [`ActorRef`]-viitteen turvalliseksi (ei `unwrap`) API:ksi.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ractor::pg;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort, SupervisionEvent};
use tracing::{debug, warn};

use familyclaw_core::{FamilyClawError, Result};

use crate::being::{BeingInfo, BeingSnapshot};
use crate::message::{BeingId, BusMessage, ResonanceMessage};

/// Oletusaikakatkaisu synkronisille `call`-kyselyille (esim. olentolista).
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Busin ohjausprotokolla — viestit, joita [`ResonanceBus`]-actor käsittelee.
///
/// Tämä on busin *sisäinen* viestityyppi (ohjaus), erotuksena olentojen
/// välisestä [`ResonanceMessage`]-liikenteestä (hyötykuorma). Useimmissa
/// tapauksissa kannattaa käyttää [`BusHandle`]-rajapintaa suoran `cast`/`call`
/// -kutsun sijaan.
pub enum BusOp {
    /// Rekisteröi olento busiin. Jos sama tunniste on jo olemassa, tiedot
    /// korvataan (uudelleenliittyminen).
    Register(BeingInfo),

    /// Poista olento busista tunnisteen perusteella.
    Deregister(BeingId),

    /// Julkaise viesti: kirjekuori toimitetaan kaikille **muille** kuin
    /// lähettäjälle. Tunnepulssi leviää samaa reittiä (affective contagion).
    Publish(ResonanceMessage),

    /// Pyydä tilannekuva liittyneistä olennoista. Vastaus palautetaan
    /// [`RpcReplyPort`]:n kautta.
    ListBeings(RpcReplyPort<Vec<BeingSnapshot>>),

    /// Pyydä liittyneiden olentojen lukumäärä.
    Count(RpcReplyPort<usize>),
}

/// `ractor::pg`-prosessiryhmien nimien yhteinen etuliite. Jokainen
/// bus-instanssi saa tästä johdetun **uniikin** ryhmänimen (ks. [`BUS_SEQ`]),
/// jotta rinnakkaiset busit eivät jaa samaa jäsenpoolia.
const PG_GROUP_PREFIX: &str = "resonance-bus";

/// Prosessin-uniikki juokseva laskuri bus-instanssien pg-ryhmänimille.
///
/// Jokainen [`ResonanceBus`] saa `pre_start`issa oman ryhmänsä
/// (`resonance-bus-{n}`), joten kahden eri busin olennot eivät koskaan näy
/// toistensa [`pg::get_members`]-tuloksessa. `ractor::pg` on prosessi-globaali
/// nimiavaruus, joten pelkkä prosessin-sisäinen uniikkius riittää — ei kelloa
/// eikä satunnaislukua tarvita.
static BUS_SEQ: AtomicU64 = AtomicU64::new(0);

/// [`ResonanceBus`]-actorin sisäinen tila: rekisteröidyt olennot.
///
/// Säilyttää HashMap-metadatan (nimet, `BeingInfo`) ListBeings-kyselyjä varten
/// ja käyttää `ractor::pg` prosessiryhmää (`pg_group`) jäsenten hallintaan ja
/// jakeluun (broadcast).
pub struct BusState {
    /// Liittyneet olennot tunnisteen mukaan indeksoituna (metatiedot).
    beings: HashMap<BeingId, BeingInfo>,
    /// Tämän bus-instanssin oma, prosessi-uniikki `ractor::pg`-ryhmän nimi.
    /// Eristää tämän busin jäsenpoolin kaikista muista prosessin buseista.
    pg_group: String,
}

impl BusState {
    /// Toimittaa kirjekuoren kaikille muille kuin lähettäjälle
    /// käyttäen ractor::pg prosessiryhmää.
    fn broadcast(&self, envelope: &ResonanceMessage, _myself: &ActorRef<BusOp>) -> usize {
        let cells = pg::get_members(&self.pg_group);
        let mut delivered = 0;
        if let Some(sender_info) = self.beings.get(&envelope.from) {
            let sender_cell = sender_info.inbox().get_cell();
            for cell in cells {
                // Skip sender by comparing ActorCell (PartialEq compares ActorId)
                if cell == sender_cell {
                    continue;
                }
                let inbox_ref: ActorRef<ResonanceMessage> = cell.clone().into();
                match inbox_ref.cast(envelope.clone()) {
                    Ok(()) => delivered += 1,
                    Err(err) => {
                        warn!(
                            being = %cell.get_id(),
                            error = %err,
                            "viestin toimitus olennolle epäonnistui (postilaatikko suljettu?)"
                        );
                    }
                }
            }
        } else {
            // Sender not in our map (shouldn't happen), send to all
            for cell in cells {
                let inbox_ref: ActorRef<ResonanceMessage> = cell.clone().into();
                match inbox_ref.cast(envelope.clone()) {
                    Ok(()) => delivered += 1,
                    Err(err) => {
                        warn!(
                            being = %cell.get_id(),
                            error = %err,
                            "viestin toimitus olennolle epäonnistui (postilaatikko suljettu?)"
                        );
                    }
                }
            }
        }
        delivered
    }
}

/// Resonance Bus -actor.
///
/// Spawnataan [`ResonanceBus::start`]-funktiolla, joka palauttaa ergonomisen
/// [`BusHandle`]:n. Actorilla ei ole konstruktiorargumentteja — tila alkaa
/// tyhjänä olentorekisterinä.
pub struct ResonanceBus;

impl Actor for ResonanceBus {
    type Msg = BusOp;
    type State = BusState;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        (): Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        // Mintataan tälle instanssille oma, prosessi-uniikki pg-ryhmänimi.
        let seq = BUS_SEQ.fetch_add(1, Ordering::Relaxed);
        let pg_group = format!("{PG_GROUP_PREFIX}-{seq}");
        Ok(BusState {
            beings: HashMap::new(),
            pg_group,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        match message {
            BusOp::Register(info) => {
                let id = info.id();
                // Handle reregister: leave old inbox from process group if exists
                if let Some(old_info) = state.beings.get(&id) {
                    pg::leave(state.pg_group.clone(), vec![old_info.inbox().get_cell()]);
                    old_info.inbox().get_cell().unlink(myself.get_cell());
                }
                // Linkitä olento busin alaiseksi (bus = supervisor)
                info.inbox().get_cell().link(myself.get_cell());
                // Liitä prosessiryhmään jakelua varten
                pg::join(state.pg_group.clone(), vec![info.inbox().get_cell()]);
                debug!(being = %id, name = info.name(), "olento rekisteröity busiin");
                state.beings.insert(id, info);
            }
            BusOp::Deregister(id) => {
                if let Some(info) = state.beings.remove(&id) {
                    info.inbox().get_cell().unlink(myself.get_cell());
                    // Poista prosessiryhmästä
                    pg::leave(state.pg_group.clone(), vec![info.inbox().get_cell()]);
                    debug!(being = %id, "olento poistettu busista");
                }
            }
            BusOp::Publish(envelope) => {
                let kind = envelope.payload.kind_label();
                let n = state.broadcast(&envelope, &myself);
                debug!(
                    from = %envelope.from,
                    kind,
                    recipients = n,
                    "viesti julkaistu busiin"
                );
            }
            BusOp::ListBeings(reply) => {
                let snapshot: Vec<BeingSnapshot> =
                    state.beings.values().map(BeingSnapshot::from).collect();
                // Vastaanottaja on saattanut antaa periksi (timeout) — älä
                // panikoi, jos portti on jo suljettu.
                if reply.send(snapshot).is_err() {
                    warn!("olentolistan vastaus hylättiin (vastaanottaja kadonnut)");
                }
            }
            BusOp::Count(reply) => {
                if reply.send(state.beings.len()).is_err() {
                    warn!("olentomäärän vastaus hylättiin (vastaanottaja kadonnut)");
                }
            }
        }
        Ok(())
    }

    /// Supervision: jos linkitetty olento kaatuu tai päättyy, poista se
    /// rekisteristä — **bus pysyy elossa**. Tämä korvaa Ractorin oletuksen
    /// (joka pysäyttäisi supervisorin lapsen kaatuessa).
    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        match &event {
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                let removed = remove_by_cell_id(state, cell.get_id());
                if let Some(id) = removed {
                    debug!(being = %id, ?reason, "olento päättyi — poistettu rekisteristä");
                }
            }
            SupervisionEvent::ActorFailed(cell, err) => {
                let removed = remove_by_cell_id(state, cell.get_id());
                if let Some(id) = removed {
                    warn!(being = %id, error = %err, "olento kaatui — poistettu rekisteristä, bus jatkaa");
                }
            }
            // Muut tapahtumat (ActorStarted, ryhmämuutokset) eivät vaadi toimia.
            _ => {}
        }
        Ok(())
    }
}

/// Poistaa rekisteristä olennon, jonka postilaatikko-actorilla on annettu
/// [`ractor::ActorId`]. Palauttaa poistetun olennon tunnisteen jos löytyi.
fn remove_by_cell_id(state: &mut BusState, cell_id: ractor::ActorId) -> Option<BeingId> {
    let found = state
        .beings
        .iter()
        .find(|(_, info)| info.inbox().get_id() == cell_id)
        .map(|(id, _)| *id);
    if let Some(id) = found {
        state.beings.remove(&id);
    }
    found
}

/// Ergonominen kahva [`ResonanceBus`]-actoriin.
///
/// Kääri raa'an [`ActorRef<BusOp>`]-viitteen API:ksi, joka:
/// - ei käytä `unwrap`/`expect`/`panic!` tuotantopolulla,
/// - muuntaa Ractor-virheet [`FamilyClawError::Bus`]-varianteiksi,
/// - tarjoaa selkeät metodit ([`register`](BusHandle::register),
///   [`publish`](BusHandle::publish), [`beings`](BusHandle::beings), …).
///
/// `BusHandle` on `Clone` — sama bus voidaan jakaa usealle olennolle.
#[derive(Clone)]
pub struct BusHandle {
    actor: ActorRef<BusOp>,
}

impl BusHandle {
    /// Käärii olemassa olevan actor-viitteen kahvaksi.
    #[must_use]
    pub fn from_ref(actor: ActorRef<BusOp>) -> Self {
        Self { actor }
    }

    /// Palauttaa alla olevan actor-viitteen (esim. olentojen linkittämiseen).
    #[must_use]
    pub fn actor_ref(&self) -> &ActorRef<BusOp> {
        &self.actor
    }

    /// Rekisteröi olennon busiin.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] jos viestin lähetys actorille epäonnistuu.
    pub fn register(&self, info: BeingInfo) -> Result<()> {
        self.actor
            .cast(BusOp::Register(info))
            .map_err(|e| FamilyClawError::bus(format!("register failed: {e}")))
    }

    /// Poistaa olennon busista tunnisteen perusteella.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] jos viestin lähetys actorille epäonnistuu.
    pub fn deregister(&self, id: BeingId) -> Result<()> {
        self.actor
            .cast(BusOp::Deregister(id))
            .map_err(|e| FamilyClawError::bus(format!("deregister failed: {e}")))
    }

    /// Julkaisee valmiin kirjekuoren busiin.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] jos viestin lähetys actorille epäonnistuu.
    pub fn publish_envelope(&self, envelope: ResonanceMessage) -> Result<()> {
        self.actor
            .cast(BusOp::Publish(envelope))
            .map_err(|e| FamilyClawError::bus(format!("publish failed: {e}")))
    }

    /// Julkaisee hyötykuorman lähettäjän puolesta (rakentaa kirjekuoren).
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] jos viestin lähetys actorille epäonnistuu.
    pub fn publish(&self, from: BeingId, payload: BusMessage) -> Result<()> {
        self.publish_envelope(ResonanceMessage::new(from, payload))
    }

    /// Palauttaa tilannekuvan liittyneistä olennoista.
    ///
    /// **Tämä lista ei ole tyhjä kun olentoja on liittynyt** (design §2.2).
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] jos kysely epäonnistuu tai aikakatkaistaan.
    pub async fn beings(&self) -> Result<Vec<BeingSnapshot>> {
        self.actor
            .call(BusOp::ListBeings, Some(DEFAULT_CALL_TIMEOUT))
            .await
            .map_err(|e| FamilyClawError::bus(format!("list beings failed: {e}")))?
            .success_or_else(|| FamilyClawError::bus("list beings: no reply (timeout)"))
    }

    /// Palauttaa liittyneiden olentojen lukumäärän.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] jos kysely epäonnistuu tai aikakatkaistaan.
    pub async fn count(&self) -> Result<usize> {
        self.actor
            .call(BusOp::Count, Some(DEFAULT_CALL_TIMEOUT))
            .await
            .map_err(|e| FamilyClawError::bus(format!("count failed: {e}")))?
            .success_or_else(|| FamilyClawError::bus("count: no reply (timeout)"))
    }

    /// Pysäyttää busin siististi.
    pub fn stop(&self) {
        self.actor.stop(None);
    }
}

impl ResonanceBus {
    /// Spawnaa Resonance Bus -actorin ja palauttaa ergonomisen [`BusHandle`]:n.
    ///
    /// `name` on valinnainen rekisteröintinimi globaalia hakua varten
    /// ([`ractor::registry`]).
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] jos actorin käynnistys epäonnistuu.
    pub async fn start(name: Option<String>) -> Result<BusHandle> {
        let (actor, _join) = Actor::spawn(name, ResonanceBus, ())
            .await
            .map_err(|e| FamilyClawError::bus(format!("bus spawn failed: {e}")))?;
        Ok(BusHandle::from_ref(actor))
    }
}

#[cfg(test)]
mod tests {
    // Testit vertaavat tarkkoja, esitettäviä f32-tunnetila-arvoja (esim. 80.0),
    // jotka kulkevat busin läpi muuttumattomina — tarkka vertailu on tässä oikein.
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::being::{CollectedLog, CollectorBeing};
    use crate::message::TaskEventKind;
    use familyclaw_emotion::{Dimension, EmotionState};
    use ractor::Actor;
    use std::time::Duration as StdDuration;

    /// Apuri: spawnaa keräävän olennon ja rekisteröi sen busiin.
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

    /// Apuri: pieni odotus, jotta asynkroninen toimitus ehtii valmistua.
    async fn settle() {
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    }

    fn log_len(log: &CollectedLog) -> usize {
        log.lock().expect("lock").len()
    }

    #[tokio::test]
    async fn beings_list_is_not_empty_after_join() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        assert_eq!(bus.count().await.expect("count"), 0);
        assert!(bus.beings().await.expect("beings").is_empty());

        let (id_a, _a, _la) = join_being(&bus, "agent_a").await;
        let (id_b, _b, _lb) = join_being(&bus, "agent_b").await;

        let beings = bus.beings().await.expect("beings");
        assert_eq!(
            beings.len(),
            2,
            "beings[] EI saa olla tyhjä kun olentoja on"
        );
        assert_eq!(bus.count().await.expect("count"), 2);

        let ids: Vec<BeingId> = beings.iter().map(|b| b.id).collect();
        assert!(ids.contains(&id_a));
        assert!(ids.contains(&id_b));

        bus.stop();
    }

    #[tokio::test]
    async fn broadcast_reaches_others_not_sender() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, log_a) = join_being(&bus, "agent_a").await;
        let (_id_b, _b, log_b) = join_being(&bus, "agent_b").await;
        let (_id_c, _c, log_c) = join_being(&bus, "agent_c").await;

        bus.publish(id_a, BusMessage::text("hei kaikki"))
            .expect("publish");
        settle().await;

        // Lähettäjä ei saa omaa viestiään kaikuna.
        assert_eq!(log_len(&log_a), 0);
        // Muut saavat sen.
        assert_eq!(log_len(&log_b), 1);
        assert_eq!(log_len(&log_c), 1);

        let received = log_b.lock().expect("lock")[0].clone();
        assert_eq!(received.from, id_a);
        assert!(matches!(received.payload, BusMessage::Text { .. }));

        bus.stop();
    }

    #[tokio::test]
    async fn emotion_pulse_spreads_to_siblings_affective_contagion() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, _la) = join_being(&bus, "agent_a").await;
        let (_id_b, _b, log_b) = join_being(&bus, "agent_b").await;

        // agent_a "luovassa virtauksessa" → pulssi vuotaa busiin.
        let mut state = EmotionState::neutral();
        state.stimulate(Dimension::Curiosity, 80.0);
        state.stimulate(Dimension::Joy, 60.0);

        bus.publish(id_a, BusMessage::emotion_pulse(state))
            .expect("publish pulse");
        settle().await;

        let received = log_b.lock().expect("lock");
        assert_eq!(received.len(), 1);
        assert!(
            received[0].is_emotion_pulse(),
            "sisarus aistii tunnepulssin"
        );
        if let BusMessage::EmotionPulse { state: got } = &received[0].payload {
            // Sisaruksen vastaanottama tila vastaa lähetettyä — contagion-data
            // on ehjä, joten vastaanottaja voi reagoida siihen.
            assert_eq!(got.value(Dimension::Curiosity), 80.0);
        } else {
            panic!("odotettiin EmotionPulse");
        }

        bus.stop();
    }

    #[tokio::test]
    async fn deregister_stops_delivery() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, _la) = join_being(&bus, "agent_a").await;
        let (id_b, _b, log_b) = join_being(&bus, "agent_b").await;

        bus.deregister(id_b).expect("deregister");
        settle().await;
        assert_eq!(bus.count().await.expect("count"), 1);

        bus.publish(id_a, BusMessage::text("vielä siellä?"))
            .expect("publish");
        settle().await;
        assert_eq!(log_len(&log_b), 0, "poistettu olento ei saa viestejä");

        bus.stop();
    }

    #[tokio::test]
    async fn task_event_broadcasts() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, _la) = join_being(&bus, "agent_a").await;
        let (_id_b, _b, log_b) = join_being(&bus, "agent_b").await;

        bus.publish(
            id_a,
            BusMessage::task_event(TaskEventKind::Completed, "task-7"),
        )
        .expect("publish task event");
        settle().await;

        let received = log_b.lock().expect("lock");
        assert_eq!(received.len(), 1);
        match &received[0].payload {
            BusMessage::TaskEvent { event, task_id, .. } => {
                assert_eq!(event, &TaskEventKind::Completed);
                assert_eq!(task_id, "task-7");
            }
            other => panic!("odotettiin TaskEvent, saatiin {other:?}"),
        }

        bus.stop();
    }

    #[tokio::test]
    async fn crashing_being_is_removed_but_bus_survives() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, log_a) = join_being(&bus, "agent_a").await;
        let (_id_b, actor_b, _lb) = join_being(&bus, "agent_b").await;

        assert_eq!(bus.count().await.expect("count"), 2);

        // Tapa toinen olento "kovasti" — simuloi kaatumista.
        actor_b.kill();

        // Anna supervision-tapahtuman levitä ja siivota rekisteri.
        settle().await;

        // Bus elää yhä ja palvelee kyselyt.
        let count = bus.count().await.expect("bus survives crash");
        assert_eq!(count, 1, "kaatunut olento poistettu, bus jatkaa");

        // Jäljellä oleva olento saa yhä viestejä.
        bus.publish(BeingId::new(), BusMessage::text("bus elossa?"))
            .expect("publish after crash");
        settle().await;
        assert_eq!(log_len(&log_a), 1);

        // Lähettäjä id_a:n osoite on yhä rekisterissä.
        let beings = bus.beings().await.expect("beings");
        assert_eq!(beings.len(), 1);
        assert_eq!(beings[0].id, id_a);

        bus.stop();
    }

    #[tokio::test]
    async fn reregister_replaces_inbox() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let id = BeingId::new();

        // Ensimmäinen postilaatikko.
        let log1 = CollectorBeing::new_log();
        let (actor1, _h1) = Actor::spawn(None, CollectorBeing, log1.clone())
            .await
            .expect("spawn 1");
        bus.register(BeingInfo::new(id, "agent_a", actor1.clone()))
            .expect("register 1");

        // Sama tunniste, uusi postilaatikko (uudelleenliittyminen).
        let log2 = CollectorBeing::new_log();
        let (actor2, _h2) = Actor::spawn(None, CollectorBeing, log2.clone())
            .await
            .expect("spawn 2");
        bus.register(BeingInfo::new(id, "agent_a", actor2.clone()))
            .expect("register 2");
        settle().await;

        assert_eq!(bus.count().await.expect("count"), 1, "ei duplikaattia");

        // Toinen olento lähettää — vain uusin postilaatikko saa viestin.
        bus.publish(BeingId::new(), BusMessage::text("kumpi saa?"))
            .expect("publish");
        settle().await;
        assert_eq!(log_len(&log1), 0, "vanha postilaatikko ei enää saa");
        assert_eq!(log_len(&log2), 1, "uusi postilaatikko saa");

        bus.stop();
    }

    #[tokio::test]
    async fn from_ref_and_actor_ref_roundtrip() {
        let bus = ResonanceBus::start(Some("named-bus".into()))
            .await
            .expect("start named bus");
        let cloned = BusHandle::from_ref(bus.actor_ref().clone());
        assert_eq!(cloned.count().await.expect("count via clone"), 0);
        bus.stop();
    }

    /// Regressiotesti: kaksi samanaikaista busia EIVÄT jaa pg-jäsenpoolia.
    ///
    /// Vanha globaali `const PG_GROUP = "resonance-bus"` -malli sai testin A
    /// olennot näkymään testin B `pg::get_members`-tuloksessa → broadcast-laskut
    /// vuotivat ristiin (rinnakkais-flakiness). Per-instanssi-ryhmänimen kanssa
    /// kummankin busin näkymä on tiukasti oma. Tämä testi epäonnistuisi vanhaa
    /// koodia vastaan.
    #[tokio::test]
    async fn two_buses_have_isolated_member_pools() {
        let bus1 = ResonanceBus::start(None).await.expect("start bus1");
        let bus2 = ResonanceBus::start(None).await.expect("start bus2");

        // Liitä kaksi olentoa busiin 1 ja kolme busiin 2.
        let (_a1, _ra1, log1_a) = join_being(&bus1, "b1_agent_a").await;
        let (id1_b, _rb1, log1_b) = join_being(&bus1, "b1_agent_b").await;
        let (_a2, _ra2, log2_a) = join_being(&bus2, "b2_agent_a").await;
        let (_b2, _rb2, log2_b) = join_being(&bus2, "b2_agent_b").await;
        let (_c2, _rc2, log2_c) = join_being(&bus2, "b2_agent_c").await;

        // Lukumäärät ovat instanssikohtaisia — eivät jaettuja.
        assert_eq!(bus1.count().await.expect("count bus1"), 2);
        assert_eq!(bus2.count().await.expect("count bus2"), 3);

        // Broadcast busissa 1 tavoittaa VAIN busin 1 muut olennot (agent_a),
        // EI busin 2 kolmea olentoa. Tämä on testin ydin: vanha globaali
        // `PG_GROUP` vuotaisi viestin busin 2 olennoille (`pg::get_members`
        // palauttaisi myös ne) → alla olevat `== 0`-väitteet kaatuisivat.
        bus1.publish(id1_b, BusMessage::text("vain perheelle 1"))
            .expect("publish bus1");
        settle().await;

        // Lähettäjä ei saa omaa viestiään kaikuna.
        assert_eq!(log_len(&log1_b), 0, "lähettäjä ei saa omaa viestiään");
        // Busin 1 toinen olento saa sen (broadcast toimii busin sisällä).
        assert_eq!(log_len(&log1_a), 1, "busin 1 sisarus saa viestin");
        // Busin 2 olennot EIVÄT saa mitään — tämä kaatuu vanhaa globaalia
        // jäsenpoolia vastaan (ristiinvuoto-regression vartija).
        assert_eq!(log_len(&log2_a), 0, "busin 2 olento ei saa busin 1 viestiä");
        assert_eq!(log_len(&log2_b), 0, "busin 2 olento ei saa busin 1 viestiä");
        assert_eq!(log_len(&log2_c), 0, "busin 2 olento ei saa busin 1 viestiä");

        bus1.stop();
        bus2.stop();
    }
}
