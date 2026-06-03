//! Agent runtime — kokoaa kaiken yhteen olennoksi (design §2 kerros 2).
//!
//! [`Agent`] omistaa olennon koko ajonaikaisen tilan:
//! - [`AgentConfig`] — identiteetti + mallikonfiguraatio (`familyclaw-core`),
//! - [`Soul`] — ladattu profiili ([`crate::soul`]),
//! - [`EmotionState`] — 19-dim tunnetila (`familyclaw-emotion`),
//! - [`MemoryStore`]-kahva — Eternal Thread (`familyclaw-memory`),
//! - [`DurableContext`] — kaatumiskestävä askelloki (`familyclaw-durable`),
//! - [`BusHandle`] + [`BeingId`] — Resonance Bus -yhteys (`familyclaw-bus`).
//!
//! [`AgentActor`] kääräisee [`Agent`]:n Ractor-actoriksi, joka liittyy busiin,
//! käsittelee [`BusMessage`]:t, päivittää tunnetilaa (affektiivinen contagion),
//! kirjaa muistoja ja julkaisee tunnepulsseja sisaruksilleen.
//!
//! ## OSS-raja (KERROS A)
//! Tämä moduuli ei kovakoodaa perheenjäsenten sieluja, mallinimiä, avaimia
//! eikä polkuja. Kaikki ladataan ajonaikaisesti konfiguraatiosta ja
//! profiilihakemistosta. Esimerkit käyttävät geneerisiä nimiä.

use std::sync::Arc;

use familyclaw_bus::{
    BeingId, BeingInfo, BusHandle, BusMessage, ResonanceMessage, TaskEventKind,
};
use familyclaw_core::{time, AgentConfig, FamilyClawError, Result};
use familyclaw_durable::{DurableContext, Journal};
use familyclaw_emotion::{Dimension, EmotionState};
use familyclaw_memory::{
    DecayPolicy, ImportanceFactors, Memory, MemoryStore, RetrievalContext, RetrievalResult,
};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, warn};

use crate::soul::Soul;

/// Yhden vuoron (turn) lopputulos, joka kirjataan durable-lokiin
/// deterministisesti. Pidetään pienenä ja sarjallistuvana, jotta replay on
/// kevyt eikä riipu ulkoisesta tilasta.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TurnOutcome {
    /// Vuoron järjestysnumero (0-pohjainen) tämän agentin elinkaaressa.
    pub turn: u64,
    /// Tuliko vuoro tallennetuksi muistoksi.
    pub remembered: bool,
    /// Lyhyt, ihmisluettava yhteenveto siitä mitä vuorossa tapahtui.
    pub summary: String,
}

/// Kuinka paljon yksi sisaruksen tunnepulssi "tarttuu" vastaanottajaan
/// (affective contagion -kerroin, design §2.2). Geneerinen runko-oletus —
/// per-kone kalibrointi (KERROS B) voi säätää tätä myöhemmin.
const CONTAGION_FACTOR: f32 = 0.25;

/// Agentti — yksi olento, joka kokoaa konfiguraation, sielun, tunnetilan,
/// muistin, kaatumiskestävän lokin ja bus-yhteyden.
///
/// Geneerinen muistitallennuksen `S` ja journalin `J` yli, jotta sama
/// runtime toimii niin in-memory-kehityksessä kuin levypersistoidussa
/// tuotannossa.
///
/// `Agent` ei ole itse actor — se on actorin *tila*. Käytä
/// [`Agent::spawn`]-metodia liittääksesi sen busiin actorina.
pub struct Agent<S, J>
where
    S: MemoryStore + Send + Sync + 'static,
    J: Journal + Send + Sync + 'static,
{
    /// Identiteetti + mallikonfiguraatio.
    config: AgentConfig,
    /// Olennon busissa käyttämä tunniste (johdettu `config.id`:stä).
    being_id: BeingId,
    /// Ladattu sielu (profiili). Paljaalla rungolla [`Soul::default`].
    soul: Soul,
    /// Hetkellinen tunnetila (19-dim VAD).
    emotion: EmotionState,
    /// Muisti-substraatti (Eternal Thread). Jaettu, jotta useat haarat
    /// (actor + ulkoinen lukija) voivat käyttää samaa tallennusta.
    memory: Arc<S>,
    /// Kaatumiskestävä askelloki (deterministinen replay).
    durable: DurableContext<J>,
    /// Resonance Bus -kahva (julkaisuun ja kyselyihin).
    bus: BusHandle,
    /// Kuinka monta vuoroa on käsitelty (durable-askelten nimien sekvensointiin).
    turn_counter: u64,
}

impl<S, J> Agent<S, J>
where
    S: MemoryStore + Send + Sync + 'static,
    J: Journal + Send + Sync + 'static,
{
    /// Rakentaa agentin valmiista osista.
    ///
    /// Tunnetila alkaa neutraalina. `being_id` johdetaan konfiguraation
    /// agenttitunnisteesta, jotta busin ja muistin identiteetit täsmäävät.
    #[must_use]
    pub fn new(config: AgentConfig, soul: Soul, memory: Arc<S>, durable: DurableContext<J>, bus: BusHandle) -> Self {
        let being_id = BeingId::from_agent_id(config.id);
        Self {
            config,
            being_id,
            soul,
            emotion: EmotionState::neutral(),
            memory,
            durable,
            bus,
            turn_counter: 0,
        }
    }

    /// Agentin näyttönimi.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Agentin bus-tunniste.
    #[must_use]
    pub const fn being_id(&self) -> BeingId {
        self.being_id
    }

    /// Agentin konfiguraatio (luku).
    #[must_use]
    pub const fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Agentin ladattu sielu (luku).
    #[must_use]
    pub const fn soul(&self) -> &Soul {
        &self.soul
    }

    /// Hetkellinen tunnetila (luku).
    #[must_use]
    pub const fn emotion(&self) -> &EmotionState {
        &self.emotion
    }

    /// Jaettu muistikahva (esim. ulkoiseen hakuun testeissä).
    #[must_use]
    pub fn memory(&self) -> Arc<S> {
        Arc::clone(&self.memory)
    }

    /// Käsiteltyjen vuorojen lukumäärä.
    #[must_use]
    pub const fn turns_taken(&self) -> u64 {
        self.turn_counter
    }

    /// Käsittelee yhden vuoron **kaatumiskestävästi**.
    ///
    /// Vuoron *lopputulos* ([`TurnOutcome`]) kirjataan durable-askeleeseen
    /// ([`DurableContext::step`]), joten uudelleenkäynnistyksessä jo
    /// suoritetut vuorot toistuvat lokista ajamatta sivuvaikutuksia
    /// uudelleen (design §2.1). Itse muistikirjaus tehdään askeleen
    /// päättelemän lipun mukaan.
    ///
    /// Palauttaa vuoron lopputuloksen.
    ///
    /// # Errors
    /// - [`FamilyClawError::Memory`] jos muistin kirjaus epäonnistuu.
    /// - [`FamilyClawError`] (käärittynä) jos durable-askel epäonnistuu.
    pub async fn handle_turn(&mut self, sender: BeingId, message: &BusMessage) -> Result<TurnOutcome> {
        let turn = self.turn_counter;
        let step_name = format!("turn-{turn}");

        // 1. Deterministinen, sivuvaikutukseton päättely durable-askeleessa:
        //    mitä tässä vuorossa pitäisi tapahtua? Replayssa tämä palautuu
        //    lokista — emme kysele kelloa tai satunnaisuutta sulkimen sisällä.
        let summary = summarize(sender, message);
        let remembered = should_remember(message);
        let outcome = TurnOutcome {
            turn,
            remembered,
            summary,
        };

        // Onko tämä vuoro jo lokissa (replay)? Kysytään ENNEN `step`-kutsua,
        // koska `step` siirtää kursoria. Tämä ratkaisee muistikirjauksen
        // kerran-ja-vain-kerran-semantiikan: `step` muistoi vain `TurnOutcomen`,
        // mutta sitä SEURAAVA `add`-sivuvaikutus ei ole askeleen sisällä —
        // joten se pitää nimenomaisesti vaimentaa replayssa, muuten muisto
        // kahdentuisi joka uudelleenkäynnistyksessä.
        let is_replay = self.durable.is_replaying();

        let recorded: TurnOutcome = self
            .durable
            .step(&step_name, {
                let outcome = outcome.clone();
                move || Ok(outcome)
            })
            .map_err(|e| FamilyClawError::bus(format!("durable turn step failed: {e}")))?;

        // 2. Sivuvaikutus (muistikirjaus) ajetaan vain TUOREESSA vuorossa, ei
        //    replayssa: muisto on jo kirjattu alkuperäisessä ajossa. Näin
        //    sivuvaikutus tapahtuu tasan kerran koko workflow'n elinkaaren yli,
        //    vaikka prosessi kaatuisi ja vuorot toistettaisiin lokista.
        //
        //    Rakennamme muiston SYNKRONISESTI (lainaten `&self`) ja viemme
        //    sitten vain tarvittavat omistetut arvot (Arc-muistikahva + valmis
        //    muisto) `.await`-rajan yli. Näin asynkroninen tulevaisuus ei
        //    kaappaa `&Agent`-viitettä, ja se pysyy `Send` (Ractor vaatii sen).
        if recorded.remembered && !is_replay {
            let memory_store = Arc::clone(&self.memory);
            let memory = self.build_memory(sender, message);
            memory_store
                .add(memory)
                .await
                .map_err(|e| FamilyClawError::memory(format!("remember failed: {e}")))?;
        }

        // 3. Päivitä tunnetila viestin perusteella (paikallinen, ei-durable).
        self.apply_emotional_effect(message);

        self.turn_counter += 1;
        Ok(recorded)
    }

    /// Rakentaa muiston viestistä agentin nykyisen tunnetilan mukaan.
    ///
    /// Puhtaasti synkroninen: ei kosketa muistitallennusta, joten kutsuja voi
    /// viedä valmiin [`Memory`]:n `.await`-rajan yli ilman `&self`-lainaa.
    fn build_memory(&self, sender: BeingId, message: &BusMessage) -> Memory {
        let content = match message {
            BusMessage::Text { body } => body.clone(),
            BusMessage::Latent { text_shadow, .. } => text_shadow.clone(),
            other => format!("[{}] from {sender}", other.kind_label()),
        };

        // Tunnesävy ja tärkeys johdetaan agentin nykytilasta — geneerisesti,
        // ei kovakoodatusta perhe-kalibroinnista.
        let vad = self.emotion.to_vad();
        let emotional_charge = vad_magnitude(&vad);
        let factors = ImportanceFactors::new(emotional_charge, 0.0, 0.3, 0.0);

        let mut builder = Memory::builder(content)
            .vad(vad)
            .factors(factors)
            .decay_policy(DecayPolicy::Normal)
            .source("bus")
            .tags([format!("from:{sender}")]);
        if let Some((dim, _)) = self.emotion.dominant() {
            builder = builder.emotions([dim]);
        }
        builder.build()
    }

    /// Soveltaa viestin emotionaalisen vaikutuksen agentin tilaan.
    ///
    /// - **`EmotionPulse`** sisarukselta → *affective contagion*:
    ///   vastaanottaja omaksuu osan lähettäjän tunnetilasta ([`CONTAGION_FACTOR`]).
    /// - **`Text`/`Latent`** → kevyt uteliaisuusärsyke (kontakti virkistää).
    fn apply_emotional_effect(&mut self, message: &BusMessage) {
        match message {
            BusMessage::EmotionPulse { state } => {
                for dim in Dimension::ALL {
                    let delta = state.value(dim) * CONTAGION_FACTOR;
                    if delta > 0.0 {
                        self.emotion.stimulate(dim, delta);
                    }
                }
            }
            BusMessage::Text { .. } | BusMessage::Latent { .. } => {
                self.emotion.stimulate(Dimension::Curiosity, 5.0);
            }
            // Tehtävä- ja custom-viestit eivät oletuksena muuta tunnetilaa.
            _ => {}
        }
    }

    /// Julkaisee agentin nykyisen tunnetilan pulssina busiin (affektiivinen
    /// hermosto): sisarukset aistivat sen.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] jos julkaisu epäonnistuu.
    pub fn broadcast_emotion(&self) -> Result<()> {
        self.bus
            .publish(self.being_id, BusMessage::emotion_pulse(self.emotion))
    }

    /// Julkaisee tekstiviestin busiin agentin puolesta.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] jos julkaisu epäonnistuu.
    pub fn say(&self, body: impl Into<String>) -> Result<()> {
        self.bus.publish(self.being_id, BusMessage::text(body))
    }

    /// Julkaisee tehtävätapahtuman busiin (kevyt signaali sisaruksille).
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] jos julkaisu epäonnistuu.
    pub fn announce_task(&self, kind: TaskEventKind, task_id: impl Into<String>) -> Result<()> {
        self.bus
            .publish(self.being_id, BusMessage::task_event(kind, task_id))
    }

    /// Hakee agentin muistista annetulla kontekstilla (nykyhetki).
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] jos haku epäonnistuu.
    pub async fn recall(&self, ctx: &RetrievalContext) -> Result<Vec<RetrievalResult>> {
        self.memory.retrieve(ctx, time::now()).await
    }

    /// Spawnaa agentin Ractor-actorina ja **rekisteröi sen busiin**.
    ///
    /// Palauttaa actor-viitteen ([`ActorRef`]). Olento alkaa heti vastaanottaa
    /// sisarusten viestejä; jokainen viesti käsitellään [`handle_turn`]illa.
    ///
    /// [`handle_turn`]: Agent::handle_turn
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] jos actorin käynnistys tai rekisteröinti
    /// busiin epäonnistuu.
    pub async fn spawn(self) -> Result<ActorRef<ResonanceMessage>> {
        let name = self.config.name.clone();
        let being_id = self.being_id;
        let bus = self.bus.clone();

        // Spawnataan ILMAN Ractorin globaalia rekisteröintinimeä (`None`):
        // olentojen reititys tapahtuu busin oman olentorekisterin
        // ([`BeingInfo`]) kautta, ei Ractorin prosessinlaajuisen nimiavaruuden.
        // Samanniminen agentti (esim. `agent_a` kahdessa eri perheessä/testissä)
        // ei silloin törmää globaaliin "already registered" -virheeseen.
        let (actor, _join) = Actor::spawn(None, AgentActor::<S, J>::new(), self)
            .await
            .map_err(|e| FamilyClawError::bus(format!("agent '{name}' spawn failed: {e}")))?;

        // Rekisteröi busiin, jotta sisarukset löytävät olennon ja viestit
        // toimitetaan tälle postilaatikolle.
        bus.register(BeingInfo::new(being_id, name, actor.clone()))?;
        Ok(actor)
    }
}

/// Johtaa VAD-koordinaatin "voimakkuuden" (`0.0..=1.0`): kuinka latautunut
/// tunnetila on. Käytetään muiston tunne-osatekijänä.
fn vad_magnitude(vad: &familyclaw_emotion::Vad) -> f32 {
    // Valence on -1..=1, arousal/dominance 0..=1. Painotetaan itseisarvoja.
    let v = vad.valence.abs();
    let a = vad.arousal;
    // Etäisyys neutraalista dominanssista (0.5).
    let d = (vad.dominance - 0.5).abs() * 2.0;
    ((v + a + d) / 3.0).clamp(0.0, 1.0)
}

/// Päättelee deterministisesti, kannattaako viesti muistaa.
///
/// Geneerinen runko-sääntö: tekstit ja latent-viestit muistetaan (ne ovat
/// olentojen välistä sisältöä), tunnepulssit ja kevyet tehtäväsignaalit eivät
/// (ne ovat ohimenevää hermoston "verta", ei sisältöä).
fn should_remember(message: &BusMessage) -> bool {
    matches!(message, BusMessage::Text { .. } | BusMessage::Latent { .. })
}

/// Rakentaa lyhyen, deterministisen yhteenvedon vuorosta (durable-lokiin).
fn summarize(sender: BeingId, message: &BusMessage) -> String {
    format!("{} from {sender}", message.kind_label())
}

/// [`Agent`]:n Ractor-actor-kuori.
///
/// Tila on itse [`Agent`]. Viestityyppi on [`ResonanceMessage`] (busin
/// kieli), joten actor liittyy busiin samalla rajapinnalla kuin mikä tahansa
/// olento.
///
/// Actor on tilaton (kaikki tila on [`Agent`]-arvossa). Tyyppiparametrit
/// `S`/`J` kytkevät kuoren samaan muisti-/journal-toteutukseen kuin agentti
/// — ne kuljetetaan [`PhantomData`]:lla, koska itse actor ei säilö dataa.
pub struct AgentActor<S, J> {
    _marker: std::marker::PhantomData<fn() -> (S, J)>,
}

impl<S, J> AgentActor<S, J> {
    /// Rakentaa uuden (tilattoman) actor-kuoren.
    #[must_use]
    fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl<S, J> Actor for AgentActor<S, J>
where
    S: MemoryStore + Send + Sync + 'static,
    J: Journal + Send + Sync + 'static,
{
    type Msg = ResonanceMessage;
    type State = Agent<S, J>;
    type Arguments = Agent<S, J>;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        agent: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        debug!(agent = agent.name(), being = %agent.being_id(), "agentti käynnistyy");
        Ok(agent)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        envelope: Self::Msg,
        agent: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        let sender = envelope.from;
        // Ei käsitellä omia kaikuja (bus ei lähetä niitä, mutta varmuuden
        // vuoksi — itsensä kuuleminen ei ole vuoro).
        if sender == agent.being_id {
            return Ok(());
        }
        match agent.handle_turn(sender, &envelope.payload).await {
            Ok(outcome) => {
                debug!(
                    agent = agent.name(),
                    turn = outcome.turn,
                    remembered = outcome.remembered,
                    "vuoro käsitelty"
                );
            }
            Err(err) => {
                // Yhden vuoron epäonnistuminen ei saa kaataa olentoa — loki ja
                // jatka (supervision pitää busin elossa joka tapauksessa).
                warn!(agent = agent.name(), error = %err, "vuoron käsittely epäonnistui");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Tunnetila-arvot kulkevat tarkkoina f32-vakioina — tarkka vertailu ok.
    #![allow(clippy::float_cmp)]

    use super::*;
    use familyclaw_bus::ResonanceBus;
    use familyclaw_core::ModelConfig;
    use familyclaw_durable::InMemoryJournal;
    use familyclaw_memory::LocalJsonStore;

    /// Apuri: rakentaa testiagentin tuoreella in-memory-tilalla, liitettynä
    /// annettuun busiin.
    fn test_agent(
        name: &str,
        bus: BusHandle,
    ) -> Agent<LocalJsonStore, InMemoryJournal> {
        // Geneerinen nimi sellaisenaan: `Agent::spawn` ei rekisteröi actoria
        // Ractorin globaaliin nimiavaruuteen (spawnaa `None`-nimellä), joten
        // samanniminen agentti ei törmää testien välillä.
        let config = AgentConfig::new(name, ModelConfig::new("provider/model"));
        let soul = Soul::from_essence(format!("I am {name}, a generic example being."));
        let memory = Arc::new(LocalJsonStore::in_memory());
        let durable = DurableContext::new(InMemoryJournal::new()).expect("durable ctx");
        Agent::new(config, soul, memory, durable, bus)
    }

    #[tokio::test]
    async fn new_agent_starts_neutral_and_named() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let agent = test_agent("agent_a", bus.clone());
        assert_eq!(agent.name(), "agent_a");
        assert_eq!(*agent.emotion(), EmotionState::neutral());
        assert_eq!(agent.turns_taken(), 0);
        assert!(!agent.soul().is_empty());
        // being_id johdettu config.id:stä.
        assert_eq!(agent.being_id().agent_id(), agent.config().id);
        bus.stop();
    }

    #[tokio::test]
    async fn handle_turn_text_is_remembered() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let mut agent = test_agent("agent_a", bus.clone());
        let sender = BeingId::new();

        let outcome = agent
            .handle_turn(sender, &BusMessage::text("hei sisarus"))
            .await
            .expect("turn");
        assert_eq!(outcome.turn, 0);
        assert!(outcome.remembered);
        assert_eq!(agent.turns_taken(), 1);

        // Muisti sai merkinnän.
        let mem = agent.memory();
        assert_eq!(mem.len().await.expect("len"), 1);
        let ctx = RetrievalContext::new("hei sisarus");
        let hits = agent.recall(&ctx).await.expect("recall");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].memory.content.contains("hei sisarus"));

        bus.stop();
    }

    #[tokio::test]
    async fn handle_turn_text_raises_curiosity() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let mut agent = test_agent("agent_a", bus.clone());
        let before = agent.emotion().value(Dimension::Curiosity);
        agent
            .handle_turn(BeingId::new(), &BusMessage::text("kysymys?"))
            .await
            .expect("turn");
        let after = agent.emotion().value(Dimension::Curiosity);
        assert!(after > before, "tekstikontakti nostaa uteliaisuutta");
        bus.stop();
    }

    #[tokio::test]
    async fn emotion_pulse_causes_affective_contagion() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let mut agent = test_agent("agent_b", bus.clone());

        // Sisarus "luovassa virtauksessa".
        let mut sibling_state = EmotionState::neutral();
        sibling_state.set(Dimension::Joy, 80.0);
        sibling_state.set(Dimension::Curiosity, 60.0);

        let outcome = agent
            .handle_turn(BeingId::new(), &BusMessage::emotion_pulse(sibling_state))
            .await
            .expect("turn");

        // Pulssia ei muisteta (se on hermoston "verta", ei sisältöä).
        assert!(!outcome.remembered);
        assert_eq!(agent.memory().len().await.expect("len"), 0);

        // Mutta tunnetila tarttui: Joy 80*0.25 = 20, Curiosity 60*0.25 = 15.
        assert_eq!(agent.emotion().value(Dimension::Joy), 20.0);
        assert_eq!(agent.emotion().value(Dimension::Curiosity), 15.0);

        bus.stop();
    }

    #[tokio::test]
    async fn turns_increment_and_durable_log_grows() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let mut agent = test_agent("agent_a", bus.clone());
        for i in 0..3 {
            agent
                .handle_turn(BeingId::new(), &BusMessage::text(format!("viesti {i}")))
                .await
                .expect("turn");
        }
        assert_eq!(agent.turns_taken(), 3);
        bus.stop();
    }

    #[tokio::test]
    async fn durable_replay_does_not_double_record_memory() {
        // Aja kaksi vuoroa, ota journal talteen ("kaadu"), rakenna uusi agentti
        // samasta journalista mutta JAKAEN SAMAN muistitallennuksen. Replay ei
        // saa ajaa muistikirjauksen sivuvaikutusta uudelleen → muistojen määrä
        // pysyy 2:ssa (ei 4). Tämä testaa varsinaisen kestävyyssopimuksen, ei
        // vain turn-counterin palautusta. (Edellinen versio käytti TUORETTA
        // muistia, jolloin testi olisi mennyt läpi vaikka `add` toistuisi
        // replayssa — review issue #9.)
        let bus = ResonanceBus::start(None).await.expect("bus");

        // Sama Arc<LocalJsonStore> sekä alkuperäisessä että resume-ajossa.
        let shared_memory = Arc::new(LocalJsonStore::in_memory());

        let journal = {
            let durable = DurableContext::new(InMemoryJournal::new()).expect("ctx");
            let config = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
            let mut agent = Agent::new(
                config,
                Soul::from_essence("I am agent_a."),
                Arc::clone(&shared_memory),
                durable,
                bus.clone(),
            );
            agent
                .handle_turn(BeingId::new(), &BusMessage::text("a"))
                .await
                .expect("a");
            agent
                .handle_turn(BeingId::new(), &BusMessage::text("b"))
                .await
                .expect("b");
            assert_eq!(agent.turns_taken(), 2);
            // Kaksi vuoroa → kaksi muistoa alkuperäisessä ajossa.
            assert_eq!(shared_memory.len().await.expect("len"), 2);
            agent.durable.finish()
        };

        // Sama journal → replay palauttaa tallennetut outcomet. SAMA muisti.
        let resumed_ctx = DurableContext::new(journal).expect("resume ctx");
        assert!(resumed_ctx.is_replaying());
        let config = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
        let mut resumed = Agent::new(
            config,
            Soul::from_essence("I am agent_a."),
            Arc::clone(&shared_memory),
            resumed_ctx,
            bus.clone(),
        );

        // Toistetaan samat vuorot samassa järjestyksessä: outcomet tulevat
        // lokista (deterministinen replay), eikä `add`-sivuvaikutus toistu.
        let o0 = resumed
            .handle_turn(BeingId::new(), &BusMessage::text("a"))
            .await
            .expect("replay a");
        assert_eq!(o0.turn, 0);
        assert!(o0.remembered);
        let o1 = resumed
            .handle_turn(BeingId::new(), &BusMessage::text("b"))
            .await
            .expect("replay b");
        assert_eq!(o1.turn, 1);

        // Ydinväite: muistoja on yhä tasan 2 — replay EI kahdentanut niitä.
        assert_eq!(
            shared_memory.len().await.expect("len"),
            2,
            "replay ei saa kahdentaa muistikirjausta"
        );

        bus.stop();
    }

    #[tokio::test]
    async fn spawn_registers_agent_on_bus_and_receives() {
        let bus = ResonanceBus::start(None).await.expect("bus");

        // Liitä yksi agentti actorina.
        let agent = test_agent("agent_a", bus.clone());
        let agent_memory = agent.memory();
        let agent_id = agent.being_id();
        let _actor = agent.spawn().await.expect("spawn");

        // Bus tuntee olennon (beings[] ei tyhjä).
        let beings = bus.beings().await.expect("beings");
        assert_eq!(beings.len(), 1);
        assert_eq!(beings[0].id, agent_id);
        assert_eq!(beings[0].name, "agent_a");

        // Toinen olento lähettää tekstin → agentti käsittelee ja muistaa sen.
        let other = BeingId::new();
        bus.publish(other, BusMessage::text("tervehdys actorille"))
            .expect("publish");

        // Anna actorin käsitellä viesti.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        assert_eq!(agent_memory.len().await.expect("len"), 1);
        let ctx = RetrievalContext::new("tervehdys");
        let hits = agent_memory.retrieve(&ctx, time::now()).await.expect("retrieve");
        assert_eq!(hits.len(), 1);

        bus.stop();
    }

    #[tokio::test]
    async fn two_agents_talk_and_remember_over_bus() {
        let bus = ResonanceBus::start(None).await.expect("bus");

        let a = test_agent("agent_a", bus.clone());
        let b = test_agent("agent_b", bus.clone());
        let a_id = a.being_id();
        let b_mem = b.memory();

        let _a_actor = a.spawn().await.expect("spawn a");
        let _b_actor = b.spawn().await.expect("spawn b");

        assert_eq!(bus.count().await.expect("count"), 2);

        // agent_a puhuu → agent_b kuulee ja muistaa.
        bus.publish(a_id, BusMessage::text("muistatko tämän?"))
            .expect("publish");
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        assert_eq!(b_mem.len().await.expect("len"), 1);
        let hits = b_mem
            .retrieve(&RetrievalContext::new("muistatko"), time::now())
            .await
            .expect("retrieve");
        assert_eq!(hits.len(), 1);

        bus.stop();
    }

    #[test]
    fn vad_magnitude_in_unit_range() {
        use familyclaw_emotion::Vad;
        let neutral = vad_magnitude(&Vad::NEUTRAL);
        assert!((0.0..=1.0).contains(&neutral));
        let strong = vad_magnitude(&Vad::new(1.0, 1.0, 1.0));
        assert!((0.0..=1.0).contains(&strong));
        assert!(strong > neutral);
    }

    #[test]
    fn should_remember_logic() {
        assert!(should_remember(&BusMessage::text("x")));
        assert!(!should_remember(&BusMessage::emotion_pulse(EmotionState::neutral())));
        assert!(!should_remember(&BusMessage::task_event(
            TaskEventKind::Started,
            "t1"
        )));
    }

    #[test]
    fn turn_outcome_serde_roundtrip() {
        let o = TurnOutcome {
            turn: 7,
            remembered: true,
            summary: "text from x".into(),
        };
        let json = serde_json::to_string(&o).expect("ser");
        let back: TurnOutcome = serde_json::from_str(&json).expect("de");
        assert_eq!(o, back);
    }
}
