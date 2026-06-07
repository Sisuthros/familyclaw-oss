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

use familyclaw_bus::{BeingId, BeingInfo, BusHandle, BusMessage, ResonanceMessage, TaskEventKind};
use familyclaw_core::{time, AgentConfig, FamilyClawError, Result};
use familyclaw_durable::{DurableContext, Journal};
use familyclaw_emotion::{Dimension, EmotionState};
use familyclaw_memory::{
    DecayPolicy, ImportanceFactors, Memory, MemoryStore, RetrievalContext, RetrievalResult,
};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, warn};

use crate::llm::{LlmClient, LlmConfig, LlmMessage};
use crate::soul::Soul;
use familyclaw_sandbox::{CodeSandbox, SandboxOutput, SandboxRequest};

/// Type-erased memory store for trait-object-based agents.
pub type ErasedMemoryStore = Arc<dyn MemoryStore + Send + Sync>;

/// Type-erased journal for trait-object-based agents.
pub type ErasedJournal = Box<dyn Journal + Send + Sync>;

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

/// Jokaisen vuoron jalkeen tunnetila palautuu talla prosentilla kohti
/// neutraalia. Arvo 0.10 (10 %) tarkoittaa: 10 vuoroa jatkuvan
/// sisaarvaikutuksen jalkeen tunnetila on vajaa puolet maksimistaan
/// (eksponentiaalinen vaimennus). Tama estaa feedback-loop-saturaation.
const HOMEOSTASIS_RATE: f32 = 0.10;

/// Agentti — yksi olento, joka kokoaa konfiguraation, sielun, tunnetilan,
/// muistin, kaatumiskestävän lokin ja bus-yhteyden.
///
/// Käyttää trait-olioita (`Box<dyn ...>`) generiikkojen sijaan, jotta
/// ulkopuoliset kehittäjät voivat rakentaa alustalle ilman monimutkaisia
/// tyyppiparametreja. Tämä on Pappa:n vaadittu "Generics-Helvetin polttaminen".
///
/// `Agent` ei ole itse actor — se on actorin *tila*. Käytä
/// [`Agent::spawn`]-metodia liittääksesi sen busiin actorina.
pub struct Agent {
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
    memory: ErasedMemoryStore,
    /// Kaatumiskestävä askelloki (deterministinen replay).
    durable: DurableContext<ErasedJournal>,
    /// Resonance Bus -kahva (julkaisuun ja kyselyihin).
    bus: BusHandle,
    /// Kuinka monta vuoroa on käsitelty (durable-askelten nimien sekvensointiin).
    turn_counter: u64,
    /// LLM-clienti ajatteluun (valinnainen, jotta testit toimivat ilman LLM:ää).
    llm: Option<LlmClient>,
    /// Sandbox koodin suorittamiseen (valinnainen, `wasmtime`-featuren kanssa).
    sandbox: Option<Arc<dyn CodeSandbox>>,
}

impl Agent {
    /// Rakentaa agentin valmiista osista.
    ///
    /// Tunnetila alkaa neutraalina. `being_id` johdetaan konfiguraation
    /// agenttitunnisteesta, jotta busin ja muistin identiteetit täsmäävät.
    /// LLM-clienti on valinnainen - jos annettu, agentti voi käyttää LLM:ää
    /// ajattelua varten (think-metodi). Sandbox on valinnainen koodin suorittamiseen.
    #[must_use]
    pub fn new(
        config: AgentConfig,
        soul: Soul,
        memory: ErasedMemoryStore,
        durable: DurableContext<ErasedJournal>,
        bus: BusHandle,
        llm_config: Option<LlmConfig>,
        sandbox: Option<Arc<dyn CodeSandbox>>,
    ) -> Self {
        let being_id = BeingId::from_agent_id(config.id);
        let llm = llm_config.map(LlmClient::new);
        Self {
            config,
            being_id,
            soul,
            emotion: EmotionState::neutral(),
            memory,
            durable,
            bus,
            turn_counter: 0,
            llm,
            sandbox,
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
    pub fn memory(&self) -> ErasedMemoryStore {
        Arc::clone(&self.memory)
    }

    /// Käsiteltyjen vuorojen lukumäärä.
    #[must_use]
    pub const fn turns_taken(&self) -> u64 {
        self.turn_counter
    }

    /// Agentin LLM-clienti (valinnainen).
    #[must_use]
    pub const fn llm(&self) -> Option<&LlmClient> {
        self.llm.as_ref()
    }

    /// Agentin sandbox (valinnainen).
    #[must_use]
    pub fn sandbox(&self) -> Option<Arc<dyn CodeSandbox>> {
        self.sandbox.clone()
    }

    /// Suorittaa koodia sandboxissa (työkalu LLM:lle).
    ///
    /// Palauttaa työkalu-vastauksen joka sisältää stdout/stderr ja fuel-kulutuksen.
    ///
    /// # Errors
    /// - [`FamilyClawError::Sandbox`] jos sandbox ei ole konfiguroitu tai suoritus epäonnistuu.
    pub fn execute_code(&self, wasm_bytes: Vec<u8>) -> Result<SandboxOutput> {
        let sandbox = self
            .sandbox
            .as_ref()
            .ok_or_else(|| FamilyClawError::sandbox("sandbox not configured"))?;

        let request = SandboxRequest::new(wasm_bytes);
        sandbox
            .execute(&request)
            .map_err(|e| FamilyClawError::sandbox(e.to_string()))
    }

    /// Agentin ajattelu: hakee relevantit muistot Eternal Threadista (RAG),
    /// rakentaa system promptin (sielu + muistit) ja kutsuu LLM:ää.
    ///
    /// Natiivisti **async** — ei `block_on`/`block_in_place`-kuvioita, jotka
    /// paniikkaisivat `current_thread`-runtimessa tai voisivat deadlockata.
    ///
    /// Palauttaa `None` jos LLM-clientiä ei ole (harmless no-op), muuten
    /// `Some(Ok(text))` tai `Some(Err(..))` LLM-virheestä.
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] jos LLM-kutsu epäonnistuu.
    #[allow(clippy::format_push_string)]
    pub async fn think(&self, current_message: &BusMessage) -> Option<Result<String>> {
        let llm = self.llm.as_ref()?;

        let query = match current_message {
            BusMessage::Text { body } => body.clone(),
            BusMessage::Latent { text_shadow, .. } => text_shadow.clone(),
            other => format!("[{}]", other.kind_label()),
        };

        // 0. ORIENT: hae relevantit muistot ENSIN (RAG — ennen LLM-kutsua).
        let recall_ctx = RetrievalContext::new(query.clone()).with_limit(5);
        let memories = self.recall(&recall_ctx).await.unwrap_or_else(|e| {
            warn!("recall failed in think (non-fatal): {e}");
            Vec::new()
        });

        // 1. System prompt: sielun ydin + muistit kontekstina.
        let mut system_prompt = self.soul.essence.clone();
        if !memories.is_empty() {
            system_prompt.push_str("\n\n[RELEVANT MEMORIES FROM ETERNAL THREAD]:\n");
            for (i, mem) in memories.iter().enumerate() {
                system_prompt.push_str(&format!(
                    "  {}. (relevance: {:.2}) {}\n",
                    i + 1,
                    mem.relevance,
                    mem.memory.content
                ));
            }
            system_prompt.push_str("[END MEMORIES]\n");
        }

        // 2. Viestit: system prompt -> nykyinen viesti.
        let messages = vec![LlmMessage::system(system_prompt), LlmMessage::user(query)];

        // 3. LLM-kutsu (async, ei block_on).
        Some(
            llm.complete(&messages)
                .await
                .map_err(|e| FamilyClawError::llm(e.to_string())),
        )
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
    pub async fn handle_turn(
        &mut self,
        sender: BeingId,
        message: &BusMessage,
    ) -> Result<TurnOutcome> {
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

        // Deterministinen paattely: turn-outcome valmis.
        // Sivuvaikutusten (muisti) idempotentti kasittely alla (kohta 2).
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
        // Idempotentti muistikirjaus: ajetaan AINA (myos replayssa),
        // koska MemoryStore::add ohittaa duplikaatit turn_key:n perusteella.
        // Tama ratkaisee dual-write-ongelman: jos durable.step onnistuu
        // mutta prossessi kaatuu ennen memory_store.add -kutsua,
        // replayssa muisto kirjataan uudelleen ja store ignooraa sen.
        if recorded.remembered {
            let memory_store = Arc::clone(&self.memory);
            let mut memory = self.build_memory(sender, message);
            memory.turn_key = Some(format!("{}:turn-{}", self.config.name, turn));
            memory_store
                .add(memory)
                .await
                .map_err(|e| FamilyClawError::memory(format!("remember failed: {e}")))?;
        }

        // 3. Paivita tunnetila viestin perusteella (paikallinen, ei-durable).
        self.apply_emotional_effect(message);

        // 4. Tunnehomeostaasi: jokaisen vuoron jalkeen tunnetila palautuu
        //    hieman kohti neutraalia. Tama estaa eksponentiaalisen saturaation
        //    (feedback loop) jaktuisissa sisaruskeskusteluissa: ilman
        //    vaimennusta CONTAGION_FACTOR kasaa tunnetiloja rajattomasti
        //    ja agentit "palaavat loppuun" muutamassa kymmenessa vuorossa.
        self.apply_emotional_homeostasis();

        // 5. LLM-ajattelu (sivuvaikutus): jos LLM-clienti on konfiguroitu,
        //    agentti "ajattelee" viestin pohjalta. LLM-generointi on ULKOINEN
        //    sivuvaikutus → ajamme sen tuoreessa vuorossa OIKEASSA async-
        //    kontekstissa (ei `block_on` durable-sulkimen sisällä, joka
        //    paniikkaisi `current_thread`-runtimessa / voisi deadlockata) ja
        //    tallennamme TULOKSEN durable-askeleeseen. Replayssa emme aja
        //    `think`:iä uudelleen — `durable.step` palauttaa tallennetun tekstin.
        let think_step = format!("{step_name}-think");
        let thought_response: Option<String> = if self.llm.is_none() {
            None
        } else if self.durable.is_replaying() {
            // Replay: palauta tallennettu LLM-vastaus lokista ilman uutta kutsua.
            self.durable
                .step(&think_step, || Ok(String::new()))
                .ok()
                .filter(|s| !s.is_empty())
        } else {
            // Tuore vuoro: aja LLM async-kontekstissa, tallenna tulos askeleeseen.
            match self.think(message).await {
                Some(Ok(text)) => self
                    .durable
                    .step(&think_step, {
                        let text = text.clone();
                        move || Ok(text)
                    })
                    .ok()
                    .filter(|s| !s.is_empty()),
                Some(Err(e)) => {
                    warn!("think failed (non-fatal): {e}");
                    None
                }
                None => None,
            }
        };

        // Liitä LLM-ajattelun tiivistelmä vuoron yhteenvetoon (jos saatu).
        let recorded = match thought_response {
            Some(thought) if !thought.is_empty() => {
                let snippet: String = thought.chars().take(160).collect();
                TurnOutcome {
                    summary: format!("{} | thought: {snippet}", recorded.summary),
                    ..recorded
                }
            }
            _ => recorded,
        };

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

    /// Tunnehomeostaasi: palauttaa jokaisen dimension hieman kohti
    /// neutraalia (`HOMEOSTASIS_RATE` * deviaatio). Tama on biologinen
    /// vastine: tunneilmaisu haihtuu ilman jatkuvaa aihetta.
    ///
    /// Esim. jos `Joy = 80` ja neutraali on 0, deviaatio = 80,
    /// palautuminen = `0.10 * 80 = 8`, uusi arvo = `72`.
    fn apply_emotional_homeostasis(&mut self) {
        for dim in Dimension::ALL {
            let current = self.emotion.value(dim);
            let neutral = EmotionState::neutral().value(dim);
            let deviation = current - neutral;
            if deviation.abs() > 0.01 {
                let correction = deviation * HOMEOSTASIS_RATE;
                let new_value = current - correction;
                self.emotion.set(dim, new_value);
            }
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
        let (actor, _join) = Actor::spawn(None, AgentActor::new(), self)
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

/// Type-erased agent for actor (no generics).
type ErasedAgent = Agent;

/// [`Agent`]:n Ractor-actor-kuori.
///
/// Tila on itse [`Agent`]. Viestityyppi on [`ResonanceMessage`] (busin
/// kieli), joten actor liittyy busiin samalla rajapinnalla kuin mikä tahansa
/// olento.
///
/// Actor on tilaton (kaikki tila on [`Agent`]-arvossa).
pub struct AgentActor {
    _marker: std::marker::PhantomData<fn() -> ErasedAgent>,
}

impl AgentActor {
    /// Rakentaa uuden (tilattoman) actor-kuoren.
    #[must_use]
    fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl Actor for AgentActor {
    type Msg = ResonanceMessage;
    type State = ErasedAgent;
    type Arguments = ErasedAgent;

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
    fn test_agent(name: &str, bus: BusHandle) -> Agent {
        // Geneerinen nimi sellaisenaan: `Agent::spawn` ei rekisteröi actoria
        // Ractorin globaaliin nimiavaruuteen (spawnaa `None`-nimellä), joten
        // samanniminen agentti ei törmää testien välillä.
        let config = AgentConfig::new(name, ModelConfig::new("provider/model"));
        let soul = Soul::from_essence(format!("I am {name}, a generic example being."));
        let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
        let durable =
            DurableContext::new(Box::new(InMemoryJournal::new()) as Box<dyn Journal + Send + Sync>)
                .expect("durable ctx");
        Agent::new(config, soul, memory, durable, bus, None, None)
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
        // Homeostaasi vahentaa 10% jokaisen vuoron jalkeen:
        // Joy 20*0.9 = 18.0, Curiosity 15*0.9 = 13.5.
        assert_eq!(agent.emotion().value(Dimension::Joy), 18.0);
        assert_eq!(agent.emotion().value(Dimension::Curiosity), 13.5);

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

        // Sama Arc<ErasedMemoryStore> sekä alkuperäisessä että resume-ajossa.
        let shared_memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());

        let journal = {
            let durable = DurableContext::new(
                Box::new(InMemoryJournal::new()) as Box<dyn Journal + Send + Sync>
            )
            .expect("ctx");
            let config = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
            let mut agent = Agent::new(
                config,
                Soul::from_essence("I am agent_a."),
                Arc::clone(&shared_memory),
                durable,
                bus.clone(),
                None,
                None,
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
            None,
            None,
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
        let hits = agent_memory
            .retrieve(&ctx, time::now())
            .await
            .expect("retrieve");
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
        assert!(!should_remember(&BusMessage::emotion_pulse(
            EmotionState::neutral()
        )));
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
