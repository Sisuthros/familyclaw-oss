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
use familyclaw_channels::OutboundMessage;
use familyclaw_core::{time, AgentConfig, FamilyClawError, Result};
use familyclaw_durable::{DurableContext, Journal};
use familyclaw_emotion::{
    default_governing_profile, ActionDecision, Dimension, EmotionActionGoverning,
    EmotionActionGovernor, EmotionState, GoverningProfile,
};
use familyclaw_memory::{
    DecayPolicy, ImportanceFactors, Memory, MemoryStore, RetrievalContext, RetrievalResult,
};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, warn};

use crate::llm::{LlmConfig, LlmMessage};
use crate::llm_chain::LlmFailover;
use crate::soul::Soul;
use familyclaw_sandbox::{CodeSandbox, SandboxOutput, SandboxRequest};

/// Type-erased memory store for trait-object-based agents.
pub type ErasedMemoryStore = Arc<dyn MemoryStore + Send + Sync>;

/// Reply-kanava (C1 Malli A): se mpsc-lähetyspää, jota Agent käyttää
/// työntääkseen LLM-vastauksen ulos kanavalle. **mpsc, EI bus** — busiin
/// julkaisu triggeröisi uuden [`Agent::handle_turn`]:n (ääretön silmukka).
///
/// Gateway omistaa vastaanottopään ([`new_reply_channel`]) ja kutsuu
/// `Channel::send`. Agent ei koskaan kutsu kanavaa suoraan.
///
/// [`UnboundedSender::send`](tokio::sync::mpsc::UnboundedSender::send) ei ole
/// async eikä lukkiudu — siksi turvallinen kutsua synkronisesta
/// [`Agent::route_reply`]:stä.
pub type ReplySink = tokio::sync::mpsc::UnboundedSender<OutboundMessage>;

/// Rakentaa reply-kanavaparin: [`ReplySink`] agentille + vastaanottopää
/// gatewaylle (C1 Malli A — gateway omistaa recv-pään ja kutsuu `Channel::send`).
#[must_use]
pub fn new_reply_channel() -> (
    ReplySink,
    tokio::sync::mpsc::UnboundedReceiver<OutboundMessage>,
) {
    tokio::sync::mpsc::unbounded_channel()
}

/// Type-erased journal for trait-object-based agents.
pub type ErasedJournal = Arc<dyn Journal + Send + Sync>;

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
    /// LLM-failover-ketju ajatteluun (valinnainen, jotta testit toimivat ilman
    /// LLM:ää). [`Agent::new`] kääräisee yhden [`LlmConfig`]:n 1-pituiseksi
    /// ketjuksi ([`LlmFailover::single`]); koko fallback-ketju kytketään
    /// [`Agent::with_failover`]:lla (esim. runtimen `build_family`). Näin
    /// [`Agent::think`] saa failoverin: jos primary kuolee (timeout/HTTP/rate),
    /// ketjun seuraavaa klienttiä kokeillaan kunnes yksi onnistuu.
    llm: Option<LlmFailover>,
    /// Sandbox koodin suorittamiseen (valinnainen, `wasmtime`-featuren kanssa).
    sandbox: Option<Arc<dyn CodeSandbox>>,
    /// Reply-kanava (C1 Malli A): minne LLM-vastaus työnnetään ulos. `None` =
    /// pudota vastaukset (nykyinen, taaksepäin-yhteensopiva käytös).
    reply_sink: Option<ReplySink>,
    /// Reply-kohde: kanavakohtainen vastausosoite (keskustelu/kanava-id), johon
    /// [`Agent::route_reply`] lähettää. `None` = ei tunnettua kohdetta
    /// (vastaukset pudotetaan vaikka sink olisi asennettu).
    ///
    /// **Huom (C2-aukko):** koska [`BusMessage`] ei tällä hetkellä kanna
    /// kanava-alkuperää (`MessageOrigin`), reply-kohde annetaan agentille
    /// erikseen ([`Agent::with_reply_target`]). Kun laajempi C2-origin-sopimus
    /// (origin-kenttä bus-viestissä) on rakennettu, kohde voidaan johtaa
    /// per-viesti käsiteltävästä viestistä. Ks. open question.
    reply_target: Option<String>,
    /// Sessio-isolaation alkuperä (F4). `None` = nykyinen jaettu-scope käytös
    /// (taaksepäin-yhteensopiva: kaikki vuorot jakavat saman muisti-scopen).
    /// `Some(origin)` → vuoron muistot tagataan
    /// [`MessageOrigin::session_tag`]:lla ja [`Agent::think`]:n recall suodattuu
    /// samalla tagilla → eri sessioiden muistot eivät vuoda toistensa
    /// kontekstiin (yksi agentti + yksi muisti, scope tagilla — ei
    /// per-sessio-instansseja).
    ///
    /// **F2-riippuvuus:** kun [`ResonanceMessage`] kantaa originin per-viesti
    /// (F2-sopimus), tämä asetetaan per-vuoro käsiteltävästä viestistä eikä
    /// staattisesti rakennusvaiheessa. Siihen asti origin annetaan
    /// [`Agent::with_session`]:lla (oikein yhdelle sessiolle/agentille).
    session: Option<crate::session::MessageOrigin>,
    /// Tunne -> toiminta -päättelijä (Phase 1 emotion governor). Oletuksena
    /// `None` → agentti toimii vanhalla tavalla (ajattelee kaikista
    /// viesteistä, ei suodata `EmotionPulse`a ulos LLM:stä). KERROS B
    /// asentaa per-olento-profiilin [`Agent::with_governor_profile`]:lla.
    ///
    /// **Phase 1 -tehtävä:** Tämä kenttä + seuraavat suodatukset
    /// (`handle_turn`:ssa) + `EmotionActionGovernor` tekevät
    /// `EmotionPulse`-signaaleista "verta" eikä LLM-syötettä, ja
    /// päättävät mitä toimintatilaa (Hesitate / Reflect / Speak /
    /// `EngageWarmly` / `ReachOut` / Initiate) agentti käyttää.
    governor: Option<Box<dyn EmotionActionGoverning + Send + Sync>>,
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
        // Yksi `LlmConfig` kääritään 1-pituiseksi failover-ketjuksi: sama
        // käytös kuin ennen (ei fallbackeja), mutta `think()` kulkee nyt
        // failover-rajapinnan läpi. Koko ketju kytketään [`with_failover`]:lla.
        let llm = llm_config.map(LlmFailover::single);
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
            reply_sink: None,
            reply_target: None,
            session: None,
            governor: None,
        }
    }

    /// Asenna reply-sink (C1 Malli A). `None` = pudota vastaukset (nykyinen
    /// käytös, taaksepäin-yhteensopiva). Palauttaa `self` ketjutusta varten,
    /// jotta [`Agent::new`]-signatuuri pysyy muuttumattomana (C1 vaatii: ei
    /// muuteta olemassa olevaa konstruktoria).
    #[must_use]
    pub fn with_reply_sink(mut self, sink: ReplySink) -> Self {
        self.reply_sink = Some(sink);
        self
    }

    /// Aseta reply-kohde (kanavakohtainen vastausosoite, johon vastaukset
    /// reititetään). Tämä on väliaikainen C2-silta kunnes [`BusMessage`] kantaa
    /// kanava-alkuperän (`MessageOrigin`) per-viesti. Palauttaa `self`
    /// ketjutusta varten.
    #[must_use]
    pub fn with_reply_target(mut self, target: impl Into<String>) -> Self {
        self.reply_target = Some(target.into());
        self
    }

    /// Aseta **sessio-isolaation alkuperä** (F4). Tämän jälkeen agentin
    /// käsittelemien vuorojen muistot tagataan
    /// [`MessageOrigin::session_tag`](crate::session::MessageOrigin::session_tag)
    /// :lla, ja [`Agent::think`]:n recall suodattuu samalla tagilla — eli vain
    /// **tämän session** muistot näkyvät kontekstina. Yksi agentti + yksi
    /// muisti riittävät: isolaatio tehdään tagilla, ei erillisillä
    /// instansseilla. `None`-tila (oletus) säilyttää jaetun scopen
    /// (taaksepäin-yhteensopiva). Palauttaa `self` ketjutusta varten.
    ///
    /// **F2-raja:** kun [`ResonanceMessage`] kantaa originin per-viesti
    /// (F2-sopimus), tämä korvautuu per-vuoro-johdolla
    /// ([`MessageOrigin::from_inbound_envelope`](crate::session::MessageOrigin::from_inbound_envelope)).
    #[must_use]
    pub fn with_session(mut self, origin: crate::session::MessageOrigin) -> Self {
        self.session = Some(origin);
        self
    }

    /// Agentin sessio-alkuperä (F4), jos asetettu.
    #[must_use]
    pub const fn session(&self) -> Option<&crate::session::MessageOrigin> {
        self.session.as_ref()
    }

    /// Kytkee **koko failover-ketjun** agentille (korvaa
    /// [`Agent::new`]:n rakentaman 1-pituisen ketjun). Käytä tätä, kun haluat
    /// primary + fallbackit: rakenna ketju [`build_llm_chain`](crate::build_llm_chain):lla
    /// ([`ModelConfig`](familyclaw_core::ModelConfig) → [`LlmFailover`]) ja anna
    /// se tähän. [`Agent::think`] yrittää sitten ketjun klienttejä
    /// järjestyksessä, kunnes yksi onnistuu (juurisyy-korjaus: primaryn kuolema
    /// ei enää tapa vuoroa).
    ///
    /// Palauttaa `self` ketjutusta varten; [`Agent::new`]-signatuuria ei
    /// muuteta (taaksepäin-yhteensopiva).
    #[must_use]
    pub fn with_failover(mut self, failover: LlmFailover) -> Self {
        self.llm = Some(failover);
        self
    }

    /// Asenna **tunne -> toiminta -governor** (Phase 1 emotion governor).
    /// `profile` on tyypillisesti KERROS B:n V130-kalibroinnista johdettu
    /// [`GoverningProfile`], mutta voit antaa minkä tahansa
    /// [`EmotionActionGoverning`]-toteutuksen (esim. mock-testi).
    ///
    /// Kun governor on asennettu, [`Agent::handle_turn_with_origin`]:
    /// - **suodattaa** `EmotionPulse`-viestit pois LLM-ajattelusta
    ///   (ne ovat "verta", eivät puhetta)
    /// - **päättää** [`ActionDecision`]:n tilannekuvasta ja **estää
    ///   reply:n** jos päätös on `Hesitate` tai `Reflect` (turvaverkko)
    /// - **estää reply:n** `Hesitate`-tilassa kokonaan
    ///
    /// Kun governoria ei ole asennettu (oletus, taaksepäin-yhteensopiva),
    /// agentti toimii kuten ennen: ajattelee kaikista viesteistä ja
    /// vastaa aina kun on LLM.
    ///
    /// Palauttaa `self` ketjutusta varten; [`Agent::new`]-signatuuria ei
    /// muuteta.
    #[must_use]
    pub fn with_governor_profile(mut self, profile: Box<dyn EmotionActionGoverning + Send + Sync>) -> Self {
        self.governor = Some(profile);
        self
    }

    /// Asenna governor käyttäen käärittyä [`GoverningProfile`]:a
    /// (yksinkertaisempi API perustapauksille — ei tarvitse
    /// `Box<dyn>`-kääreitä käsin).
    #[must_use]
    pub fn with_governing_profile(mut self, profile: GoverningProfile) -> Self {
        self.governor = Some(Box::new(profile));
        self
    }

    /// Asenna **oletus-governor** (konservatiivinen `default_governing_profile`).
    /// Sama kuin `with_governing_profile(default_governing_profile())`,
    /// lyhyempi.
    #[must_use]
    pub fn with_default_governor(mut self) -> Self {
        self.governor = Some(Box::new(default_governing_profile()));
        self
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

    /// Agentin LLM-failover-ketju (valinnainen).
    #[must_use]
    pub const fn llm(&self) -> Option<&LlmFailover> {
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
        //    F4 sessio-isolaatio: jos sessio on asetettu, vaadi session-tag →
        //    vain TÄMÄN session muistot näkyvät kontekstina (ei vuotoa toisesta
        //    keskustelusta). Ilman sessiota (None) recall on jaettu (nykyinen,
        //    taaksepäin-yhteensopiva käytös).
        let mut recall_ctx = RetrievalContext::new(query.clone()).with_limit(5);
        if let Some(origin) = self.session.as_ref() {
            recall_ctx = recall_ctx.with_required_tags([origin.session_tag()]);
        }
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

    /// Käsittelee yhden vuoron **kaatumiskestävästi** (ilman per-viesti-
    /// alkuperää — käyttää staattista reply-kohdetta jos asetettu).
    ///
    /// Tämä on taaksepäin-yhteensopiva kuori
    /// [`handle_turn_with_origin`](Self::handle_turn_with_origin):lle
    /// `origin = None`:lla. Reply ohjautuu agentin staattiseen
    /// [`with_reply_target`](Self::with_reply_target)-kohteeseen.
    ///
    /// # Errors
    /// - [`FamilyClawError::Memory`] jos muistin kirjaus epäonnistuu.
    /// - [`FamilyClawError`] (käärittynä) jos durable-askel epäonnistuu.
    pub async fn handle_turn(
        &mut self,
        sender: BeingId,
        message: &BusMessage,
    ) -> Result<TurnOutcome> {
        self.handle_turn_with_origin(sender, message, None).await
    }

    /// Käsittelee yhden vuoron **kaatumiskestävästi**, per-viesti-alkuperän
    /// ([`familyclaw_bus::MessageOrigin`]) kanssa (F2).
    ///
    /// Vuoron *lopputulos* ([`TurnOutcome`]) kirjataan durable-askeleeseen
    /// ([`DurableContext::step`]), joten uudelleenkäynnistyksessä jo
    /// suoritetut vuorot toistuvat lokista ajamatta sivuvaikutuksia
    /// uudelleen (design §2.1). Itse muistikirjaus tehdään askeleen
    /// päättelemän lipun mukaan.
    ///
    /// ## Reply-kohteen johto (F2-ydin)
    /// Vastauksen kohde johdetaan **per viesti**: jos `origin` on annettu, kohde
    /// on `origin.reply_target()` (se keskustelu, josta viesti tuli). Muuten
    /// palataan agentin staattiseen [`with_reply_target`](Self::with_reply_target)
    /// -arvoon. Näin yksi agentti voi palvella montaa keskustelua ilman että
    /// vastaukset vuotavat väärään kohteeseen — eikä yhden kanavan +
    /// staattisen kohteen MVP-käytös rikkoonnu (`origin = None` → entinen polku).
    ///
    /// Palauttaa vuoron lopputuloksen.
    ///
    /// # Errors
    /// - [`FamilyClawError::Memory`] jos muistin kirjaus epäonnistuu.
    /// - [`FamilyClawError`] (käärittynä) jos durable-askel epäonnistuu.
    // Vuoronkäsittely on yhtenäinen, peräkkäinen prosessi; pilkkominen vain
    // rivimäärän takia hajottaisi loogisen kokonaisuuden.
    #[allow(clippy::too_many_lines)]
    pub async fn handle_turn_with_origin(
        &mut self,
        sender: BeingId,
        message: &BusMessage,
        origin: Option<&familyclaw_bus::MessageOrigin>,
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
        //
        //    **Phase 1 governor -suodatus:** Kun governor on asennettu JA
        //    viesti on `EmotionPulse` (sisarusten "verta", ei puhetta), EI
        //    ajatella lainkaan. Tämä estää turhat LLM-kutsut affektiivisissa
        //    pulssiketjuissa ja varmistaa että vain puhutut viestit tuottavat
        //    LLM-vastauksen. Tämä on nimenomainen korjaus yhteen tärkeimmistä
        //    Phase 1 -aukoista (pitfall listalla).
        let think_step = format!("{step_name}-think");
        let governor_filtered_pulse = self.governor.is_some()
            && matches!(message, BusMessage::EmotionPulse { .. });
        let governor_hesitate = self
            .governor
            .as_deref()
            .is_some_and(|g| {
                let gov = EmotionActionGovernor::new(g);
                gov.decide(&self.emotion) == ActionDecision::Hesitate
            });
        let thought_response: Option<String> = if self.llm.is_none() {
            None
        } else if governor_filtered_pulse {
            // Phase 1: EmotionPulse = "verta", ei ajatella. Kirjaa lokiin
            // ettei replayssa ajeta, mutta tässä turnissa think palauttaa None.
            debug!(
                agent = self.config.name,
                "governor: skipping think() for EmotionPulse (filtered as 'blood', not speech)"
            );
            self.durable
                .step(&think_step, || Ok(String::new()))
                .ok()
                .filter(|s| !s.is_empty())
        } else if governor_hesitate {
            // Phase 1: turvaveto (fear/anger/shame yli katon) → ei ajatella
            // LLM:ää tällä vuorolla. Kirjaa lokiin.
            debug!(
                agent = self.config.name,
                "governor: Hesitate decision blocks think() (safety veto)"
            );
            self.durable
                .step(&think_step, || Ok(String::new()))
                .ok()
                .filter(|s| !s.is_empty())
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

        // 5b. Reply-path (C1 Malli A, TEHTÄVÄ C2): jos `think()` tuotti tekstin
        //     JA reply-sink + reply-kohde on asennettu, työnnä vastaus ULOS
        //     kanavalle. Tämä on ERI polku kuin bus-julkaisu — gateway omistaa
        //     recv-pään ja kutsuu `Channel::send`. EMME julkaise busiin
        //     (ääretön-silmukka-suoja: bus-reply triggeröisi uuden
        //     handle_turn:n). Ajetaan VAIN tuoreessa vuorossa, ei replayssa:
        //     ulkomaailmaan lähetys on idempotentittömyysraja (kahdentaisi
        //     viestin käyttäjälle), joten replay ei saa toistaa sitä.
        //
        //     **Phase 1 governor -portinvartija:** Kun governor on asennettu
        //     JA päätös on `Hesitate`, EI vastata ollenkaan. Tämä on
        //     kriittinen turvaverkko: tulvinut agentti (korkea fear/anger)
        //     ei pääse lähettämään tuhoisaa reply:tä ennen kuin tilanne
        //     tasaantuu. Sama pätee `Reflect`-tilaan (agentti miettii
        //     sisäisesti eikä puhu, vaikka LLM olisi tuottanut tekstin).
        let reply_decision_blocks = self
            .governor
            .as_deref()
            .and_then(|g| {
                let gov = EmotionActionGovernor::new(g);
                match gov.decide(&self.emotion) {
                    ActionDecision::Hesitate | ActionDecision::Reflect => {
                        debug!(
                            agent = self.config.name,
                            "governor: Hesitate/Reflect decision blocks reply (silenced)"
                        );
                        Some(())
                    }
                    _ => None,
                }
            });
        if !self.durable.is_replaying() && reply_decision_blocks.is_none() {
            if let Some(thought) = thought_response.as_deref().filter(|s| !s.is_empty()) {
                // F2: johda reply-kohde per viesti. Origin ENSIN (se keskustelu
                // josta viesti tuli), FALLBACK staattiseen reply-kohteeseen.
                // Näin >1 keskustelu reitittyy oikein, ja yhden kanavan +
                // staattisen kohteen MVP-käytös säilyy (origin = None).
                let target: Option<&str> = origin
                    .map(familyclaw_bus::MessageOrigin::reply_target)
                    .or(self.reply_target.as_deref());
                if let Some(target) = target {
                    match OutboundMessage::new(target, thought) {
                        Ok(reply) => {
                            if let Err(e) = self.route_reply(reply) {
                                // Reitityksen epäonnistuminen (suljettu sink) ei
                                // saa kaataa vuoroa — loki ja jatka.
                                warn!("reply routing failed (non-fatal): {e}");
                            }
                        }
                        // Tyhjä target/body torjutaan jo aiemmin; varmuuden vuoksi.
                        Err(e) => warn!("reply build failed (non-fatal): {e}"),
                    }
                }
            }
        }

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

        // F4 sessio-isolaatio: tagaa muisto session-tagilla kun sessio on
        // asetettu, jotta [`Agent::think`]:n recall voi suodattaa per-sessio.
        // Ilman sessiota (None) vain `from:`-tag → jaettu scope (nykyinen
        // käytös, taaksepäin-yhteensopiva).
        let mut tags = vec![format!("from:{sender}")];
        if let Some(origin) = self.session.as_ref() {
            tags.push(origin.session_tag());
        }
        let mut builder = Memory::builder(content)
            .vad(vad)
            .factors(factors)
            .decay_policy(DecayPolicy::Normal)
            .source("bus")
            .tags(tags);
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
                    // Affektiivinen contagion *lähestymisenä*, ei kasauksena:
                    // vastaanottaja liikkuu lähteen tunnetilaa kohti osuudella
                    // CONTAGION_FACTOR. Koska delta lasketaan EROSTA
                    // (lähde − vastaanottaja), arvo ei voi koskaan ylittää
                    // lähdettä eikä saturoida kattoon — jokainen pulssi
                    // pienenee kun arvot lähestyvät toisiaan. Tämä korjaa
                    // code review #2:n "tuotannon kaataja"-bugin, jossa
                    // `lähde * CONTAGION_FACTOR` -kasaus + 10 %:n homeostaasi
                    // tasapainottui arvoon `2.25 * lähde` → saturaatio kattoon.
                    let current = self.emotion.value(dim);
                    let delta = (state.value(dim) - current) * CONTAGION_FACTOR;
                    self.emotion.stimulate(dim, delta);
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

    /// Reitittää vastauksen **ulos kanavalle** reply-sinkin kautta (C1 Malli A).
    ///
    /// Tämä on **eri polku** kuin [`Agent::say`]/[`Agent::broadcast_emotion`]:
    /// ne julkaisevat busiin (sisarukset kuulevat), kun taas `route_reply`
    /// työntää viestin mpsc-kanavaan, jonka gateway omistaa ja jonka kautta
    /// `Channel::send` kutsutaan ulkomaailmaan. **Ei bus-julkaisua** —
    /// bus-reply triggeröisi uuden [`Agent::handle_turn`]:n (ääretön silmukka).
    ///
    /// No-op (palauttaa `Ok`) jos reply-sinkiä ei ole asennettu — tämä on
    /// taaksepäin-yhteensopiva oletuskäytös (vastaukset pudotetaan).
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] jos sink on asennettu mutta vastaanottopää on
    /// suljettu (gateway lopetti) — vastausta ei voitu toimittaa.
    pub fn route_reply(&self, msg: OutboundMessage) -> Result<()> {
        match self.reply_sink.as_ref() {
            Some(sink) => sink
                .send(msg)
                .map_err(|e| FamilyClawError::bus(format!("reply sink closed: {e}"))),
            // Ei sinkiä = pudota vastaus (nykyinen käytös, taaksepäin-yht.sop.).
            None => Ok(()),
        }
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
        // F2: per-viesti-alkuperä kirjekuoresta → reply-kohde johdetaan per
        // viesti (origin.reply_target()), fallback staattiseen kohteeseen.
        let origin = envelope.origin.clone();
        match agent
            .handle_turn_with_origin(sender, &envelope.payload, origin.as_ref())
            .await
        {
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
            DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
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
                Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>
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

    // ---- C2 reply-path (C1 Malli A) -------------------------------------

    /// Ydinväite (TEHTÄVÄ C2): kun reply-sink + reply-kohde on asennettu,
    /// agentin tuottama vastaus päätyy reply-sinkiin OIKEALLA kohteella
    /// (channel/conversation-id). Tämä on se sama polku, jonka `handle_turn`
    /// ajaa kun `think()` tuottaa tekstin: rakenna `OutboundMessage` kohteella
    /// → `route_reply` → gateway saa sen recv-päästä.
    #[tokio::test]
    async fn route_reply_reaches_sink_with_correct_target() {
        let bus = ResonanceBus::start(None).await.expect("bus");

        let (sink, mut rx) = new_reply_channel();
        let agent = test_agent("agent_a", bus.clone())
            .with_reply_sink(sink)
            .with_reply_target("discord:general-42");

        // Sama rakennuslogiikka kuin handle_turn:in reply-path-haarassa:
        // think-teksti → OutboundMessage agentin reply-kohteella.
        let thought = "ajattelin tämän";
        let reply = OutboundMessage::new("discord:general-42", thought).expect("reply");
        agent.route_reply(reply).expect("route");

        // Gateway (recv-pää) sai vastauksen oikealla channel/conversation-id:llä.
        let got = rx.recv().await.expect("reply delivered");
        assert_eq!(got.target, "discord:general-42", "vastaus oikeaan kanavaan");
        assert_eq!(got.body, thought);

        bus.stop();
    }

    /// Ilman reply-sinkiä `route_reply` on no-op (palauttaa Ok) — nykyinen,
    /// taaksepäin-yhteensopiva käytös (vastaukset pudotetaan).
    #[tokio::test]
    async fn route_reply_without_sink_is_noop() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let agent = test_agent("agent_a", bus.clone());
        let reply = OutboundMessage::new("anywhere", "ei kuulijaa").expect("reply");
        // Ei paniikkia, ei virhettä — vastaus vain pudotetaan.
        agent.route_reply(reply).expect("noop ok");
        bus.stop();
    }

    /// Jos sink on asennettu mutta gateway sulki recv-pään, `route_reply`
    /// palauttaa Err (vastausta ei voitu toimittaa) — eikä paniikkaa.
    #[tokio::test]
    async fn route_reply_errors_when_sink_closed() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let (sink, rx) = new_reply_channel();
        drop(rx); // gateway lopetti → recv-pää suljettu.
        let agent = test_agent("agent_a", bus.clone()).with_reply_sink(sink);
        let reply = OutboundMessage::new("c", "hukkaan").expect("reply");
        assert!(
            agent.route_reply(reply).is_err(),
            "suljettu sink → toimitusvirhe"
        );
        bus.stop();
    }

    // ---- F1 failover-wiring ---------------------------------------------

    /// `Agent::new(Some(LlmConfig))` kääräisee yhden klientin 1-pituiseksi
    /// failover-ketjuksi (taaksepäin-yhteensopiva: ei fallbackeja).
    #[tokio::test]
    async fn new_with_llm_config_wraps_single_failover() {
        use crate::llm::LlmConfig;
        let bus = ResonanceBus::start(None).await.expect("bus");
        let config = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
        let soul = Soul::from_essence("I am agent_a.");
        let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
        let durable =
            DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
                .expect("durable");
        let llm_cfg = LlmConfig::new("http://localhost:9/v1", "k", "single-model");
        let agent = Agent::new(
            config,
            soul,
            memory,
            durable,
            bus.clone(),
            Some(llm_cfg),
            None,
        );

        let failover = agent.llm().expect("llm wired");
        assert_eq!(failover.len(), 1, "yksi config → 1-pituinen ketju");
        assert_eq!(failover.primary_model(), "single-model");
        bus.stop();
    }

    /// `with_failover` korvaa konstruktorin 1-pituisen ketjun KOKO ketjulla
    /// (primary + fallbackit) — F1: agentti saa failoverin, ei vain primaryä.
    #[tokio::test]
    async fn with_failover_replaces_chain_with_full_failover() {
        use crate::llm_chain::{build_llm_chain, EnvEndpointResolver};
        let bus = ResonanceBus::start(None).await.expect("bus");
        let resolver = EnvEndpointResolver::new()
            .with_provider("openai", "https://api.openai.com/v1", "OPENAI_API_KEY")
            .with_provider(
                "deepseek",
                "https://api.deepseek.com/v1",
                "DEEPSEEK_API_KEY",
            );
        let model = ModelConfig::new("openai/gpt-4o").with_fallback("deepseek/deepseek-v4-pro");
        let chain = build_llm_chain(&model, &resolver).expect("chain builds");

        // Agentti rakennetaan ILMAN llm:ää, sitten kytketään koko ketju.
        let agent = test_agent("agent_a", bus.clone()).with_failover(chain);
        let failover = agent.llm().expect("failover wired");
        assert_eq!(failover.len(), 2, "primary + 1 fallback");
        assert_eq!(failover.primary_model(), "openai/gpt-4o");
        bus.stop();
    }

    // ---- F4 sessio-isolaatio --------------------------------------------

    use crate::session::MessageOrigin;

    /// F4 write-side: kun sessio on asetettu, vuoron muisto saa session-tagin
    /// (`session:<channel>:<conversation>`) `from:`-tagin lisäksi. Ilman
    /// sessiota tagia ei ole (jaettu scope säilyy).
    #[tokio::test]
    async fn session_tags_memory_for_isolation() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let origin = MessageOrigin::new("discord-main", "general", "user-1");
        let mut agent = test_agent("agent_a", bus.clone()).with_session(origin.clone());

        agent
            .handle_turn(BeingId::new(), &BusMessage::text("sessio-viesti"))
            .await
            .expect("turn");

        // Muisto on tagattu session-tagilla → recall samalla vaaditulla tagilla
        // löytää sen.
        let scoped =
            RetrievalContext::new("sessio-viesti").with_required_tags([origin.session_tag()]);
        let hits = agent.recall(&scoped).await.expect("recall scoped");
        assert_eq!(
            hits.len(),
            1,
            "session-tagilla suodatettu recall löytää muiston"
        );
        assert!(hits[0].memory.tags.contains(&origin.session_tag()));

        bus.stop();
    }

    /// F4 read-side (ydinväite): kahden eri session muistot **eivät vuoda**
    /// toistensa kontekstiin. Sama jaettu muisti, mutta vaadittu session-tag
    /// erottaa A:n muistot B:n hausta.
    #[tokio::test]
    async fn sessions_do_not_leak_memories_across_each_other() {
        let bus = ResonanceBus::start(None).await.expect("bus");

        // JAETTU muisti (yksi store) — todistaa että isolaatio tulee tagista,
        // ei erillisistä storeista.
        let shared: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());

        let origin_a = MessageOrigin::new("discord-main", "channel-a", "u");
        let origin_b = MessageOrigin::new("discord-main", "channel-b", "u");

        // Sessio A kirjoittaa muiston jaettuun storeen.
        {
            let durable = DurableContext::new(
                Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>
            )
            .expect("durable");
            let mut agent_a = Agent::new(
                AgentConfig::new("agent_a", ModelConfig::new("provider/model")),
                Soul::from_essence("I am agent_a."),
                Arc::clone(&shared),
                durable,
                bus.clone(),
                None,
                None,
            )
            .with_session(origin_a.clone());
            agent_a
                .handle_turn(BeingId::new(), &BusMessage::text("salaisuus kanavasta A"))
                .await
                .expect("turn a");
        }

        // Sessio B kirjoittaa OMAN muistonsa SAMAAN storeen. Eri agentti-nimi
        // ("agent_b") → eri turn_key → muisti-store ei deduplikoi sitä A:n
        // turn-0:n kanssa (dedup on per-agentti turn_key, ei per-sessio).
        let durable_b =
            DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
                .expect("durable");
        let mut agent_b = Agent::new(
            AgentConfig::new("agent_b", ModelConfig::new("provider/model")),
            Soul::from_essence("I am agent_b."),
            Arc::clone(&shared),
            durable_b,
            bus.clone(),
            None,
            None,
        )
        .with_session(origin_b.clone());
        agent_b
            .handle_turn(BeingId::new(), &BusMessage::text("viesti kanavasta B"))
            .await
            .expect("turn b");

        // Jaettu store sisältää MOLEMMAT muistot.
        assert_eq!(shared.len().await.expect("len"), 2);

        // B:n session-scope (vaadittu B-tag) EI näe A:n muistoa.
        let b_scope = RetrievalContext::new("salaisuus kanavasta A")
            .with_required_tags([origin_b.session_tag()]);
        let b_sees = agent_b.recall(&b_scope).await.expect("recall b");
        assert!(
            b_sees
                .iter()
                .all(|r| !r.memory.content.contains("kanavasta A")),
            "B:n sessio ei saa nähdä A:n muistoa"
        );

        // A:n session-scope näkee A:n oman muiston (positiivinen kontrolli).
        let a_scope = RetrievalContext::new("salaisuus kanavasta A")
            .with_required_tags([origin_a.session_tag()]);
        let a_sees = agent_b.recall(&a_scope).await.expect("recall a");
        assert_eq!(a_sees.len(), 1, "A:n sessio näkee oman muistonsa");
        assert!(a_sees[0].memory.content.contains("kanavasta A"));

        bus.stop();
    }

    /// Ilman sessiota (None) recall on jaettu — taaksepäin-yhteensopiva
    /// negatiivinen kontrolli: nykyinen MVP-käytös säilyy muuttumattomana.
    #[tokio::test]
    async fn no_session_keeps_shared_scope() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let mut agent = test_agent("agent_a", bus.clone());
        assert!(agent.session().is_none(), "oletus: ei sessiota");

        agent
            .handle_turn(BeingId::new(), &BusMessage::text("jaettu viesti"))
            .await
            .expect("turn");

        // Recall ILMAN tagivaatimusta löytää muiston (jaettu scope).
        let hits = agent
            .recall(&RetrievalContext::new("jaettu viesti"))
            .await
            .expect("recall");
        assert_eq!(hits.len(), 1);
        // Muistossa ei ole session-tagia (ei `session:`-etuliitettä).
        assert!(
            hits[0]
                .memory
                .tags
                .iter()
                .all(|t| !t.starts_with(crate::session::SESSION_TAG_PREFIX)),
            "ilman sessiota muisto ei saa session-tagia"
        );
        bus.stop();
    }

    /// `with_reply_sink` / `with_reply_target` ketjuttuvat eivätkä muuta
    /// `Agent::new`-signatuuria (C1: konstruktoria ei kosketa).
    #[tokio::test]
    async fn reply_setters_chain_and_preserve_identity() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let (sink, _rx) = new_reply_channel();
        let agent = test_agent("agent_a", bus.clone())
            .with_reply_sink(sink)
            .with_reply_target("tg:chat-7");
        // Identiteetti säilyy setterien jälkeen.
        assert_eq!(agent.name(), "agent_a");
        assert_eq!(agent.turns_taken(), 0);
        bus.stop();
    }

    /// Phase 1: Kun governoria ei ole asennettu, agentti toimii
    /// taaksepäin-yhteensopivasti (oletuskäytös säilyy).
    #[tokio::test]
    async fn no_governor_means_legacy_behavior() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let mut agent = test_agent("agent_a", bus.clone());
        // Oletusarvoisesti governor-kenttä on None → perustila.
        // Käsittele teksti → muistetaan (sama kuin ennen governoria).
        let outcome = agent
            .handle_turn(BeingId::new(), &BusMessage::text("vanha viesti"))
            .await
            .expect("turn");
        assert!(outcome.remembered);
        bus.stop();
    }

    /// Phase 1: Default-governor suodattaa EmotionPulse-viestit pois
    /// LLM-ajattelusta. Tämä on keskeinen korjaus: emotion-pulssit ovat
    /// "verta" eivät puhetta, eivätkä saa triggeröidä LLM-kutsua.
    #[tokio::test]
    async fn default_governor_filters_emotion_pulse_from_think() {
        use familyclaw_emotion::EmotionState;
        let bus = ResonanceBus::start(None).await.expect("bus");
        // Agentti, jossa on default-governor (mutta EI LLM:ää, joten
        // voimme tarkistaa että suodatus ei kaada).
        let mut agent = test_agent("agent_a", bus.clone())
            .with_default_governor();
        // Simuloi "pelokas" tila, jotta LLM EI suodattaisi
        // (governor_decide olisi Hesitate), mutta silti saamme testin
        // kattamaan EmotionPulse-polun. Annetaan tilalle neutraali.
        agent.emotion = EmotionState::neutral();
        // EmotionPulse sisarukselta → pitäisi palauttaa onnistunut turn
        // ilman kaatumista. (LLM:ää ei ole → thought_response = None, mutta
        // polku menee governor-suodatuksen läpi.)
        let outcome = agent
            .handle_turn(BeingId::new(), &BusMessage::emotion_pulse(EmotionState::neutral()))
            .await
            .expect("turn should not fail when governor filters");
        // Pulssia ei muisteta (se on "verta", ei sisältöä).
        assert!(!outcome.remembered);
        bus.stop();
    }

    /// Phase 1: Default-governor tuottaa Hesitate-päätöksen kun
    /// turvakynnys ylittyy (Fear yli 80), mikä estää reply:n.
    /// Tämä testaa portinvartijaa: vaikka LLM tuottaisi tekstin,
    /// reply:tä ei lähetetä Hesitate-tilassa.
    #[tokio::test]
    async fn governor_hesitate_blocks_reply() {
        use familyclaw_emotion::{Dimension, EmotionState};
        let bus = ResonanceBus::start(None).await.expect("bus");
        let (sink, mut rx) = new_reply_channel();
        // Asenna governor + reply-target. LLM:ää ei tarvita testiin;
        // testaamme vain että Hesitate-tila estää reply-polun.
        let mut agent = test_agent("agent_a", bus.clone())
            .with_default_governor()
            .with_reply_sink(sink)
            .with_reply_target("tg:chat-7");
        // Pakotetaan "pelokas" tunnetila.
        let mut fear_state = EmotionState::neutral();
        fear_state.set(Dimension::Fear, 95.0);
        agent.emotion = fear_state;
        // Tekstiviesti → handle_turn etenee, mutta reply pitäisi estää
        // koska governor päättää Hesitate.
        let _ = agent
            .handle_turn(BeingId::new(), &BusMessage::text("scary"))
            .await
            .expect("turn");
        // Reply-kanava EI saisi sisältää viestejä.
        let received = rx.try_recv();
        assert!(
            received.is_err(),
            "Hesitate-tilassa reply:tä ei saa lähettää, saatiin: {received:?}"
        );
        bus.stop();
    }

    /// Regressiotesti (code review #2, "tuotannon kaataja"): jatkuva
    /// korkea sisaruspulssi EI saa ajaa vastaanottajan dimensiota kattoon
    /// (100). Ennen korjausta contagion lisäsi `source * 0.25` joka tikki
    /// riippumatta vastaanottajan arvosta → homeostaasi (10 %) ei ehtinyt
    /// vaimentaa ja tasapaino oli `2.25 * source` → saturaatio kattoon.
    /// Korjauksen jälkeen contagion lähestyy lähdettä (`(source - target) *
    /// factor`), joten arvo ei voi ylittää lähdettä eikä saturoidu kattoon.
    #[tokio::test]
    async fn repeated_contagion_does_not_saturate_to_ceiling() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let mut agent = test_agent("agent_sat", bus.clone());

        // Sisarus jatkuvassa korkeassa ilossa (mutta EI katossa: 80/100).
        let mut sibling_state = EmotionState::neutral();
        sibling_state.set(Dimension::Joy, 80.0);

        // Sata vuoroa samaa korkeaa pulssia — pahin tapaus feedback-loopille.
        for _ in 0..100 {
            agent
                .handle_turn(BeingId::new(), &BusMessage::emotion_pulse(sibling_state))
                .await
                .expect("turn");
        }

        let joy = agent.emotion().value(Dimension::Joy);
        // Ei saturaatiota kattoon: pysyy reilusti alle 100.
        assert!(
            joy < 100.0,
            "jatkuva contagion ei saa saturoida kattoon, joy = {joy}"
        );
        // Eikä saa ylittää lähteen arvoa (contagion = lähestyminen, ei kasaus).
        assert!(
            joy <= 80.0 + 1e-3,
            "vastaanottaja ei saa ylittää lähdettä (80), joy = {joy}"
        );
    }

    /// Kun korkeat sisaruspulssit loppuvat, homeostaasi vetää tunnetilan
    /// takaisin kohti neutraalia (baseline 0) — ei jää jumiin kohonneeseen
    /// arvoon. Todistaa että decay/homeostaasi-termi tasapainottaa contagionin.
    #[tokio::test]
    async fn homeostasis_pulls_back_toward_baseline_after_contagion() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let mut agent = test_agent("agent_decay", bus.clone());

        // Nosta tunnetila contagionilla muutamalla pulssilla.
        let mut sibling_state = EmotionState::neutral();
        sibling_state.set(Dimension::Joy, 80.0);
        for _ in 0..5 {
            agent
                .handle_turn(BeingId::new(), &BusMessage::emotion_pulse(sibling_state))
                .await
                .expect("turn");
        }
        let elevated = agent.emotion().value(Dimension::Joy);
        assert!(elevated > 0.0, "contagion nosti iloa, joy = {elevated}");

        // Pulssit loppuvat → neutraalit (tunnetilaa muuttamattomat) vuorot.
        // Task-viesti ei muuta tunnetilaa (vain homeostaasi ajetaan).
        for _ in 0..30 {
            agent
                .handle_turn(
                    BeingId::new(),
                    &BusMessage::task_event(TaskEventKind::Started, "noop"),
                )
                .await
                .expect("turn");
        }
        let relaxed = agent.emotion().value(Dimension::Joy);
        // Homeostaasi veti takaisin kohti baselinea (0) — selvästi alaspäin.
        assert!(
            relaxed < elevated,
            "homeostaasin pitäisi laskea iloa: {elevated} -> {relaxed}"
        );
        // 30 vuoroa 10 %:n eksponentiaalista vaimennusta → murto-osa
        // alkuperäisestä. Robusti suhteellinen raja (ei herkkä tarkalle
        // contagion/decay-aritmetiikalle): vähintään 90 % palautunut.
        assert!(
            relaxed < elevated * 0.1,
            "pitkän tauon jälkeen ilon pitäisi olla lähellä baselinea: \
             {elevated} -> {relaxed}"
        );

        bus.stop();
    }

    /// Phase 1: `with_governor_profile` ottaa `Box<dyn>` -rajapinnan,
    /// joten KERROS B voi syöttää oman per-being-profiilin.
    #[tokio::test]
    async fn with_governor_profile_accepts_dyn() {
        use familyclaw_emotion::default_governing_profile;
        let bus = ResonanceBus::start(None).await.expect("bus");
        let mut agent = test_agent("agent_a", bus.clone());
        let profile: Box<dyn familyclaw_emotion::EmotionActionGoverning + Send + Sync> =
            Box::new(default_governing_profile());
        agent = agent.with_governor_profile(profile);
        // Tunnistaminen: agentin tulee nyt noudattaa governoria.
        // Yksinkertainen tarkistus: turn etenee onnistuneesti.
        let outcome = agent
            .handle_turn(BeingId::new(), &BusMessage::text("ok"))
            .await
            .expect("turn");
        assert!(outcome.remembered);
        bus.stop();
    }
}
