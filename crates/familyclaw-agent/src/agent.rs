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

use familyclaw_actions::{ActionRuntime, McpToolDescriptor};
use familyclaw_bus::{BeingId, BeingInfo, BusHandle, BusMessage, ResonanceMessage, TaskEventKind};
use familyclaw_channels::OutboundMessage;
use familyclaw_core::{time, AgentConfig, FamilyClawError, Result};
use tokio::sync::Mutex;
use familyclaw_durable::{DurableContext, Journal};
use familyclaw_emotion::{
    default_governing_profile, ActionDecision, Dimension, EmotionActionGoverning,
    EmotionActionGovernor, EmotionCalibration, EmotionState, GoverningProfile, NeutralCalibration,
};
use familyclaw_memory::{
    DecayPolicy, ImportanceFactors, Memory, MemoryStore, RetrievalContext, RetrievalResult,
};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, warn};

use crate::llm::{LlmConfig, LlmMessage, ToolDefinition};
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

/// Tool-loopin (Phase 1 keystone) konfiguraatio.
///
/// Rajoittaa kuinka monta kertaa [`Agent::think`] saa kiertää
/// (LLM-kutsu → työkalukutsu → tulos takaisin → uusi LLM-kutsu) ennen kuin
/// silmukka pysäytetään pakolla. Tämä on **turvaraja**, ei tavoite: hyvin
/// käyttäytyvä malli pysähtyy itse kun se lakkaa pyytämästä työkaluja
/// (ks. [`Agent::think`]). Raja takaa että huonosti käyttäytyvä tai
/// looppaava malli ei jää ikuiseen kiertoon eikä polta budjettia loputtomiin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolLoopConfig {
    /// Suurin sallittu kierrosmäärä (LLM-kutsut) yhden vuoron aikana.
    /// Jokainen työkalukutsu — myös tuntematon — kuluttaa yhden kierroksen,
    /// joten raja sitoo silmukan vaikka malli pyytäisi vain virheellisiä
    /// työkaluja. Oletus [`ToolLoopConfig::DEFAULT_MAX_ITERATIONS`].
    pub max_iterations: u32,
}

impl ToolLoopConfig {
    /// Oletuskierrosraja: kahdeksan LLM-kutsua per vuoro. Riittää tyypilliseen
    /// monivaiheiseen työkalusarjaan jättämättä silmukkaa rajattomaksi.
    pub const DEFAULT_MAX_ITERATIONS: u32 = 8;
}

impl Default for ToolLoopConfig {
    /// Oletus: [`ToolLoopConfig::DEFAULT_MAX_ITERATIONS`] kierrosta.
    fn default() -> Self {
        Self {
            max_iterations: Self::DEFAULT_MAX_ITERATIONS,
        }
    }
}

/// Tool-loopin **lopputulos** (Phase 1 keystone).
///
/// Erottaa toisistaan *oikean vastauksen* käyttäjälle ([`ToolLoopOutcome::Answer`])
/// ja kaksi *ei-vastausta olevaa ohjaustilaa* — odottaa hyväksyntää
/// ([`ToolLoopOutcome::AwaitingApproval`]) ja kierrosraja täyttyi
/// ([`ToolLoopOutcome::MaxIterations`]). Vain `Answer` saa ylittää
/// käyttäjärajan (reply-kanava + durable-yhteenveto); ohjaustilat ovat
/// **kehittäjälle sisäisiä**, eikä niiden merkkijonoesitystä koskaan reititetä
/// loppukäyttäjälle eikä tallenneta vuoron yhteenvetoon.
///
/// Tämä korjaa Phase 1 -aukon jossa väliaikaiset merkit (mm. raaka
/// `approval_id`) vuotivat sanatarkasti käyttäjälle normaalin reply-putken
/// kautta. Täysiluokkainen suspend-paluu (`ThinkOutcome::Suspended`) tulee 1C:ssä;
/// tämä tyyppi pitää rajan ehjänä siihen asti.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolLoopOutcome {
    /// Malli pysähtyi lopulliseen vastaukseen → reititetään käyttäjälle.
    Answer(String),
    /// Työkalu vaatii ihmisen hyväksynnän → suoritus jäi odottamaan. Sisäinen
    /// ohjaustila: hyväksynnän tunniste ([`familyclaw_actions::ApprovalId`])
    /// elää [`ActionRuntime`]:ssa operaattorin myöhempää `approve`-kutsua
    /// varten — sitä EI lähetetä käyttäjälle.
    AwaitingApproval {
        /// Hyväksyntää vaatineen työkalun nimi (loki/diagnostiikka).
        tool: String,
        /// Myönnetyn hyväksynnän tunniste (vain sisäiseen lokitukseen).
        approval_id: String,
    },
    /// Kierrosraja täyttyi ilman lopullista vastausta → ei käyttäjäreplyä.
    MaxIterations {
        /// Saavutettu kierrosraja (loki/diagnostiikka).
        iterations: u32,
    },
}

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
    /// Tunnemoottorin kalibrointi (KERROS B -profiilidata, ladataan
    /// ajonaikaisesti `calibration.json`:sta). Oletuksena
    /// [`NeutralCalibration`] → tunnetila vetää kohti nollaa neutraalilla
    /// decay-nopeudella (täysin taaksepäin-yhteensopiva entiseen kovakoodattuun
    /// käytökseen). Kun ei-neutraali kalibrointi asennetaan
    /// ([`Agent::with_calibration`]), se muuttaa:
    /// - **homeostaasin lepotilaa** ([`Agent::apply_emotional_homeostasis`]):
    ///   tunne palautuu kohti dimension `baseline`-arvoa, ei aina nollaa;
    /// - **ärsykeherkkyyttä** ([`Agent::apply_emotional_effect`]): kontakti-
    ///   ärsyke skaalataan dimension `sensitivity`-kertoimella.
    ///
    /// Koska governor lukee `self.emotion`-tilaa, kalibrointi vaikuttaa myös
    /// governorin [`ActionDecision`]:eihin epäsuorasti (eri tila → eri päätös).
    calibration: Box<dyn EmotionCalibration + Send + Sync>,
    /// Toimintoajoympäristö tool-loopia varten (Phase 1 keystone). Oletuksena
    /// `None` → [`Agent::think`] säilyttää vanhan **yhden kerran** -käytöksen
    /// (yksi LLM-kutsu, ei työkaluja). Kun
    /// [`with_actions`](Agent::with_actions) asentaa
    /// [`ActionRuntime`]:n, `think()` ajaa tool-loopin: rakentaa
    /// työkalumääritelmät runtimen julkaisemista MCP-kuvauksista, antaa ne
    /// LLM:lle ja reitittää mallin valitsemat työkalukutsut takaisin
    /// runtimeen kunnes malli lakkaa pyytämästä työkaluja (tai raja täyttyy).
    ///
    /// Sisäinen muuttuvuus ([`Mutex`]): [`ActionRuntime::submit_task`] on
    /// `&mut self`, mutta `think()` lainaa `&self` (Ractor-actor jakaa tilan).
    /// `Arc<Mutex<…>>` antaa useamman haaran (actor + ulkoinen kutsu) jakaa
    /// saman runtimen turvallisesti `.await`-rajojen yli.
    actions: Option<Arc<Mutex<ActionRuntime>>>,
    /// Tool-loopin turvaraja (kierrosmäärä per vuoro). Käytössä vain kun
    /// [`actions`](Agent::actions) on asennettu; muuten yhden kerran -polku ei
    /// kierrä lainkaan. Oletus [`ToolLoopConfig::default`].
    tool_loop: ToolLoopConfig,
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
            calibration: Box::new(NeutralCalibration),
            actions: None,
            tool_loop: ToolLoopConfig::default(),
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

    /// **Jatka ELÄVÄNÄ durable-replayn päältä** (gateway-restart-korjaus).
    ///
    /// Kun agentti rakennetaan olemassa olevan journalin päälle, sen
    /// durable-konteksti on replay-tilassa: jokainen aiemmin kirjattu
    /// `turn-{n}` (+ `turn-{n}-think`) on replay-vektorissa. Gateway kuitenkin
    /// palvelee **uusia eläviä viestejä** — se EI syötä historiaa uudelleen.
    /// Ilman tätä kutsua seuraava elävä vuoro:
    /// 1. käyttäisi `turn_counter = 0`:aa → askelnimi `turn-0`, joka osuisi yhä
    ///    avoinna olevaan replay-haaraan ja kaatuisi
    ///    [`DurableError::NondeterministicReplay`](familyclaw_durable::DurableError::NondeterministicReplay):hin (tai mykistäisi vuoron,
    ///    koska `is_replaying()` gatettaa LLM-ajattelun ja reply:n), ja
    /// 2. törmäisi muistin `turn_key`:ssä (`{name}:turn-0`) replayn duplikaattiin
    ///    → uuden viestin muisti häviäisi (`MemoryStore` dedup).
    ///
    /// Tämä builder tekee KAKSI toisiinsa kytkettyä asiaa, jotka on tehtävä
    /// yhdessä:
    /// - **siirtää durable-kursorin replayn loppuun**
    ///   ([`DurableContext::fast_forward_replay`](familyclaw_durable::DurableContext::fast_forward_replay))
    ///   → seuraava askel menee tuore-ajo-haaraan oikealla sekvenssipaikalla, ja
    ///   `is_replaying()` on `false` → agentti ajattelee ja vastaa taas, ja
    /// - **palauttaa `turn_counter`:n** seuraavaan vapaaseen vuoropaikkaan
    ///   ([`DurableContext::replayed_turn_count`](familyclaw_durable::DurableContext::replayed_turn_count))
    ///   → uusi vuoro on `turn-{N}` (uniikki nimi + uniikki `turn_key`).
    ///
    /// Asetetaan **vain persistentillä, elävällä polulla** (runtimen
    /// `build_family`, kun `FAMILYCLAW_DATA_DIR` on asetettu). In-memory-polku
    /// (replay tyhjä → no-op) ja in-order-uudelleensyöttö (continuity-daemon /
    /// replay-testit, jotka syöttävät saman historian järjestyksessä) EIVÄT
    /// kutsu tätä — ne haluavat replayn täsmäävän askel askeleelta.
    ///
    /// Palauttaa `self` ketjutusta varten ([`Agent::new`]-signatuuri ei muutu).
    #[must_use]
    pub fn resume_live(mut self) -> Self {
        self.turn_counter = self.durable.replayed_turn_count();
        self.durable.fast_forward_replay();
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
    pub fn with_governor_profile(
        mut self,
        profile: Box<dyn EmotionActionGoverning + Send + Sync>,
    ) -> Self {
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

    /// Asenna **tunnemoottorin kalibrointi** (KERROS B -profiilidata).
    /// `calibration` on tyypillisesti
    /// [`TableCalibration`](familyclaw_emotion::TableCalibration), joka on
    /// ladattu agentin `calibration.json`:sta
    /// ([`TableCalibration::from_path`](familyclaw_emotion::TableCalibration::from_path)).
    ///
    /// Kun ei-neutraali kalibrointi on asennettu, tunnetila palautuu kohti
    /// dimension `baseline`-lepotilaa (ei aina nollaa) ja kontaktiärsykkeet
    /// skaalataan dimension `sensitivity`-kertoimella. Ilman tätä (oletus,
    /// [`NeutralCalibration`]) agentti toimii kuten ennen — täysin
    /// taaksepäin-yhteensopiva.
    ///
    /// Palauttaa `self` ketjutusta varten; [`Agent::new`]-signatuuria ei
    /// muuteta.
    #[must_use]
    pub fn with_calibration(
        mut self,
        calibration: Box<dyn EmotionCalibration + Send + Sync>,
    ) -> Self {
        self.calibration = calibration;
        self
    }

    /// Agentin tunnemoottorin kalibroinnin tunnistettava nimi (lokitusta varten).
    #[must_use]
    pub fn calibration_label(&self) -> &str {
        self.calibration.label()
    }

    /// Asenna **toimintoajoympäristö** tool-loopia varten (Phase 1 keystone).
    ///
    /// Kun runtime on asennettu, [`Agent::think`] vaihtaa **yhden kerran**
    /// -polusta tool-looppiin: se rakentaa runtimen julkaisemista
    /// [`McpToolDescriptor`]-kuvauksista LLM:lle tarjottavat
    /// [`ToolDefinition`]:t, antaa ne mallille ja reitittää mallin valitsemat
    /// työkalukutsut takaisin runtimeen ([`ActionRuntime::submit_task`]) kunnes
    /// malli lakkaa pyytämästä työkaluja tai [`ToolLoopConfig`]-raja täyttyy.
    ///
    /// **Additiivinen + taaksepäin-yhteensopiva:** ilman tätä kutsua
    /// (`actions = None`) `think()` toimii täsmälleen kuten ennen — yksi
    /// LLM-kutsu, ei työkaluja. Olemassa olevat polut (gateway, testit)
    /// säilyvät muuttumattomina kunnes runtime asennetaan eksplisiittisesti.
    ///
    /// `runtime` annetaan jaettuna ([`Arc`] + [`Mutex`]), koska
    /// [`ActionRuntime::submit_task`] on `&mut self` mutta `think()` lainaa
    /// `&self`. Palauttaa `self` ketjutusta varten ([`Agent::new`]-signatuuria
    /// ei muuteta).
    #[must_use]
    pub fn with_actions(mut self, runtime: Arc<Mutex<ActionRuntime>>) -> Self {
        self.actions = Some(runtime);
        self
    }

    /// Säädä **tool-loopin turvaraja** (kierrosmäärä per vuoro). Käytössä vain
    /// kun [`with_actions`](Agent::with_actions) on asennettu. Palauttaa `self`
    /// ketjutusta varten.
    #[must_use]
    pub const fn with_tool_loop(mut self, config: ToolLoopConfig) -> Self {
        self.tool_loop = config;
        self
    }

    /// Agentin tool-loop-konfiguraatio (luku).
    #[must_use]
    pub const fn tool_loop(&self) -> ToolLoopConfig {
        self.tool_loop
    }

    /// Onko agentille asennettu toimintoajoympäristö (tool-loop aktiivinen)?
    #[must_use]
    pub const fn has_actions(&self) -> bool {
        self.actions.is_some()
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
    /// ## Kaksi polkua (Phase 1 keystone)
    /// - **`actions = None` (oletus, ENNALLAAN):** yksi LLM-kutsu
    ///   ([`LlmFailover::complete`]) ilman työkaluja. Tämä on alkuperäinen
    ///   yhden kerran -käytös, jota ei muuteta — kaikki vanhat polut ja testit
    ///   säilyvät.
    /// - **`actions = Some(rt)`:** sisäinen `run_tool_loop` ajaa silmukan: LLM
    ///   saa runtimen julkaisemat työkalut, ja sen valitsemat työkalukutsut
    ///   reititetään takaisin runtimeen kunnes malli vastaa ilman
    ///   työkalukutsuja (pysähdys) tai [`ToolLoopConfig`]-raja täyttyy.
    ///
    /// Palauttaa `None` jos LLM-clientiä ei ole (harmless no-op), muuten
    /// `Some(Ok(text))` tai `Some(Err(..))` LLM-virheestä.
    ///
    /// ## Käyttäjärajan suojaus (tool-loop)
    /// Tool-loop-polulla `think` palauttaa **vain oikean vastauksen** käyttäjälle
    /// (`Some(Ok(text))`). Kaksi ei-vastausta olevaa ohjaustilaa —
    /// odottaa-hyväksyntää ja kierrosraja-täynnä — kirjataan lokiin ja
    /// palautetaan `None`. Näin niiden sisäisiä merkkijonoja (mm. raaka
    /// `approval_id`) **ei koskaan reititetä loppukäyttäjälle** eikä tallenneta
    /// vuoron yhteenvetoon — `None` tarkoittaa "ei vastausta tällä vuorolla",
    /// jonka [`handle_turn_with_origin`](Self::handle_turn_with_origin) suodattaa
    /// jo nyt pois reply-putkesta.
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] jos LLM-kutsu epäonnistuu.
    pub async fn think(&self, current_message: &BusMessage) -> Option<Result<String>> {
        let llm = self.llm.as_ref()?;
        let (system_prompt, query) = self.build_think_context(current_message).await;

        match self.actions.as_ref() {
            // Yhden kerran -polku (taaksepäin-yhteensopiva): yksi LLM-kutsu,
            // ei työkaluja. Sama käytös kuin ennen tool-loopia.
            None => {
                let messages =
                    vec![LlmMessage::system(system_prompt), LlmMessage::user(query)];
                Some(
                    llm.complete(&messages)
                        .await
                        .map_err(|e| FamilyClawError::llm(e.to_string())),
                )
            }
            // Tool-loop-polku: anna mallille työkalut ja kierrä kunnes se
            // lakkaa pyytämästä niitä (tai raja täyttyy). Vain `Answer` ylittää
            // käyttäjärajan; ohjaustilat suodatetaan `None`:ksi (loki alla).
            Some(actions) => match self.run_tool_loop(llm, actions, system_prompt, query).await {
                Err(e) => Some(Err(e)),
                Ok(ToolLoopOutcome::Answer(text)) => Some(Ok(text)),
                Ok(ToolLoopOutcome::AwaitingApproval { tool, approval_id }) => {
                    // Sisäinen ohjaustila: työkalu odottaa ihmisen hyväksyntää.
                    // EI käyttäjälle — `approval_id` on operaattorin (ActionRuntime)
                    // tieto, ei vastaus. Phase 1: vaikenemme tällä vuorolla.
                    debug!(
                        agent = self.config.name,
                        tool = tool.as_str(),
                        approval_id = approval_id.as_str(),
                        "tool loop: awaiting human approval — suppressing internal marker from user reply"
                    );
                    None
                }
                Ok(ToolLoopOutcome::MaxIterations { iterations }) => {
                    // Sisäinen ohjaustila: raja täyttyi ilman vastausta. EI
                    // robottimaista max-iter-merkkiä käyttäjälle.
                    debug!(
                        agent = self.config.name,
                        iterations,
                        "tool loop: reached max iterations without a final answer — suppressing internal marker from user reply"
                    );
                    None
                }
            },
        }
    }

    /// Rakentaa [`think`](Agent::think):n jaetun kontekstin: RAG-recall +
    /// system prompt (sielun ydin + muistit) sekä viestin teksti (`query`).
    ///
    /// Jaettu molempien polkujen (yhden kerran + tool-loop) kesken, jotta
    /// muistihaku ja promptin rakennus ovat identtiset riippumatta siitä onko
    /// työkaluja asennettu. F4 sessio-isolaatio: jos sessio on asetettu,
    /// recall vaatii session-tagin (vain tämän session muistot näkyvät).
    #[allow(clippy::format_push_string)]
    async fn build_think_context(&self, current_message: &BusMessage) -> (String, String) {
        let query = match current_message {
            BusMessage::Text { body } => body.clone(),
            BusMessage::Latent { text_shadow, .. } => text_shadow.clone(),
            other => format!("[{}]", other.kind_label()),
        };

        // ORIENT: hae relevantit muistot ENSIN (RAG — ennen LLM-kutsua).
        let mut recall_ctx = RetrievalContext::new(query.clone()).with_limit(5);
        if let Some(origin) = self.session.as_ref() {
            recall_ctx = recall_ctx.with_required_tags([origin.session_tag()]);
        }
        let memories = self.recall(&recall_ctx).await.unwrap_or_else(|e| {
            warn!("recall failed in think (non-fatal): {e}");
            Vec::new()
        });

        // System prompt: sielun ydin + muistit kontekstina.
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

        (system_prompt, query)
    }

    /// Ajaa **tool-loopin** (Phase 1 keystone): SpatialClaw-tyylinen silmukka
    /// jossa malli kutsuu työkalun, tulos syötetään takaisin, ja kierretään
    /// pysähdykseen.
    ///
    /// ## Vaiheet per kierros
    /// 1. Rakenna työkalumääritelmät runtimen julkaisemista MCP-kuvauksista
    ///    ([`ActionRuntime::tool_definitions`] → [`ToolDefinition`]) — vain
    ///    kelvolliset ([`ToolDefinition::validate`]) tarjotaan mallille.
    /// 2. Kutsu [`LlmFailover::complete_with_tools`].
    /// 3. **Ei työkalukutsuja** → palauta mallin teksti (pysähdys).
    /// 4. Jokaiselle työkalukutsulle:
    ///    - **tuntematon työkalu** ([`ActionRuntime::map_name_to_skill`] = `None`)
    ///      → työnnä virhe-`tool_result` ja JATKA (kuluttaa kierroksen, ei
    ///      keskeytä eikä jää ikuiseen yritykseen),
    ///    - **hyväksyntää vaativa** ([`SubmitOutcome::pending_approval`] = `Some`)
    ///      → palauta [`ToolLoopOutcome::AwaitingApproval`] (sisäinen ohjaustila,
    ///      EI käyttäjälle; täysiluokkainen `ThinkOutcome::Suspended` tulee 1C:ssä),
    ///    - **turvallinen / auto-run** → työnnä tulos `tool_result`:na ja jatka.
    /// 5. **Raja täyttyy** → palauta [`ToolLoopOutcome::Answer`] (viimeisin teksti)
    ///    tai [`ToolLoopOutcome::MaxIterations`] (ei vastausta).
    ///
    /// ## Käyttäjäraja
    /// Vain [`ToolLoopOutcome::Answer`] on tarkoitettu loppukäyttäjälle.
    /// Ohjaustilat ([`ToolLoopOutcome::AwaitingApproval`],
    /// [`ToolLoopOutcome::MaxIterations`]) ovat kehittäjälle sisäisiä — niiden
    /// erottelu tyypissä estää sen että väliaikainen merkki (mm. raaka
    /// `approval_id`) vuotaisi reply-putken läpi käyttäjälle.
    ///
    /// Ei koskaan paniikkaa: kaikki virhepolut palautuvat [`Result`]:na tai
    /// jatkavat silmukkaa rajan sisällä.
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] jos LLM-kutsu epäonnistuu palautumattomasti.
    async fn run_tool_loop(
        &self,
        llm: &LlmFailover,
        actions: &Arc<Mutex<ActionRuntime>>,
        system_prompt: String,
        query: String,
    ) -> Result<ToolLoopOutcome> {
        let mut messages = vec![LlmMessage::system(system_prompt), LlmMessage::user(query)];
        // Viimeisin mallin tuottama teksti — palautetaan jos raja täyttyy
        // ennen kuin malli pysähtyy itse.
        let mut last_text = String::new();

        for _ in 0..self.tool_loop.max_iterations {
            // 1. Rakenna työkalut runtimen MCP-kuvauksista (lukko vain
            //    kuvausten ajaksi — vapautetaan ennen LLM-kutsua).
            let tools = {
                let rt = actions.lock().await;
                build_tool_definitions(&rt.tool_definitions())
            };

            // 2. LLM-kutsu työkaluineen.
            let result = llm
                .complete_with_tools(&messages, &tools)
                .await
                .map_err(|e| FamilyClawError::llm(e.to_string()))?;

            if !result.text().is_empty() {
                last_text = result.text().to_string();
            }

            // 3. Ei työkalukutsuja → malli pysähtyi, palauta teksti vastauksena.
            //    Tyhjä mutta läsnä oleva content (`Some("")`, jota osa
            //    OpenAI-yhteensopivista providereista tuottaa) suodatetaan pois,
            //    jotta palautamme aiemman ei-tyhjän tekstin emmekä mykisty.
            let Some(tool_calls) = result.tool_calls.filter(|c| !c.is_empty()) else {
                let answer = result.content.filter(|c| !c.is_empty()).unwrap_or(last_text);
                return Ok(ToolLoopOutcome::Answer(answer));
            };

            // Liitä mallin assistant-vuoro (työkalukutsuineen) historiaan, jotta
            // seuraavat tool_result-viestit sitoutuvat oikeisiin call-id:eihin.
            messages.push(
                LlmMessage::assistant(result.content.unwrap_or_default())
                    .with_tool_calls(tool_calls.clone()),
            );

            // 4. Dispatchaa jokainen työkalukutsu ja syötä tulos takaisin.
            for call in tool_calls {
                let Some(skill_id) = actions.lock().await.map_name_to_skill(&call.name) else {
                    // Tuntematon työkalu: virhe-tulos takaisin, JATKA (kuluttaa
                    // kierroksen, ei keskeytä silmukkaa eikä jää loputtomaan
                    // yritykseen — raja sitoo myös virhepolun).
                    debug!(
                        agent = self.config.name,
                        tool = call.name.as_str(),
                        "tool loop: unknown tool — feeding error result, continuing"
                    );
                    messages.push(LlmMessage::tool_result(
                        call.id,
                        format!("error: unknown tool '{}'", call.name),
                    ));
                    continue;
                };

                let outcome = {
                    let mut rt = actions.lock().await;
                    rt.submit_task(skill_id, call.arguments.clone(), time::now())
                        .await
                };

                match outcome {
                    Ok(submit) if submit.pending_approval.is_some() => {
                        // Hyväksyntää vaativa työkalu → palautetaan SISÄINEN
                        // ohjaustila [`ToolLoopOutcome::AwaitingApproval`], EI
                        // käyttäjälle reititettävää merkkijonoa. `approval_id`
                        // jää [`ActionRuntime`]:n tilaan operaattorin myöhempää
                        // `approve`-kutsua varten ja kulkee vain sisäiseen
                        // lokitukseen — sitä ei koskaan lähetetä loppukäyttäjälle.
                        // Vuoro ei jää roikkumaan eikä hyväksyntää vaativa toiminto
                        // suoriudu ilman lupaa. (Täysiluokkainen
                        // `ThinkOutcome::Suspended` tulee 1C:ssä.)
                        let approval_id = submit
                            .pending_approval
                            .map_or_else(|| "?".to_string(), |id| id.to_string());
                        return Ok(ToolLoopOutcome::AwaitingApproval {
                            tool: call.name,
                            approval_id,
                        });
                    }
                    Ok(submit) => {
                        // Turvallinen / auto-run: syötä (redaktoitu) tulos
                        // takaisin malliin. Todiste sisältää redaktoidun
                        // tulosteen ilman salaisuuksia.
                        let result_text = {
                            let rt = actions.lock().await;
                            tool_result_text(&rt, &submit)
                        };
                        messages.push(LlmMessage::tool_result(call.id, result_text));
                    }
                    Err(e) => {
                        // Suoritusvirhe: syötä virhe takaisin malliin (se voi
                        // korjata kutsun), JATKA rajan sisällä.
                        warn!(
                            agent = self.config.name,
                            tool = call.name.as_str(),
                            error = %e,
                            "tool loop: submit_task failed — feeding error result, continuing"
                        );
                        messages.push(LlmMessage::tool_result(
                            call.id,
                            format!("error: tool '{}' failed: {e}", call.name),
                        ));
                    }
                }
            }
        }

        // 5. Raja täyttyi ennen pysähdystä. EI paniikkia. Jos malli ehti
        //    tuottaa tekstiä, se on paras saatavilla oleva vastaus → `Answer`.
        //    Muuten palautetaan SISÄINEN ohjaustila [`ToolLoopOutcome::MaxIterations`]
        //    — robottimaista max-iter-merkkiä EI reititetä käyttäjälle.
        if last_text.is_empty() {
            Ok(ToolLoopOutcome::MaxIterations {
                iterations: self.tool_loop.max_iterations,
            })
        } else {
            Ok(ToolLoopOutcome::Answer(last_text))
        }
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
        let governor_filtered_pulse =
            self.governor.is_some() && matches!(message, BusMessage::EmotionPulse { .. });
        let governor_hesitate = self.governor.as_deref().is_some_and(|g| {
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
        let reply_decision_blocks = self.governor.as_deref().and_then(|g| {
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
                // Kontakti virkistää uteliaisuutta. Kalibroinnin
                // `sensitivity` skaalaa ärsykkeen voimakkuutta (KERROS B:n
                // viritys); neutraalisti kerroin on 1.0 → entinen +5.0.
                let sensitivity = self.calibration.sensitivity(Dimension::Curiosity);
                self.emotion
                    .stimulate(Dimension::Curiosity, 5.0 * sensitivity);
            }
            // Tehtävä- ja custom-viestit eivät oletuksena muuta tunnetilaa.
            _ => {}
        }
    }

    /// Tunnehomeostaasi: palauttaa jokaisen dimension hieman kohti
    /// kalibroinnin **lepotilaa** (`HOMEOSTASIS_RATE` * deviaatio
    /// baselinesta, skaalattuna dimension `decay_rate`-kertoimella). Tama on
    /// biologinen vastine: tunneilmaisu haihtuu ilman jatkuvaa aihetta.
    ///
    /// Neutraalilla kalibroinnilla `baseline = 0`, `decay_rate = 1` → entinen
    /// käytös (esim. `Joy = 80`, lepotila 0, deviaatio 80, palautuminen
    /// `0.10 * 80 = 8`, uusi arvo `72`). KERROS B:n kalibrointi voi vetää
    /// dimension kohti ei-nollaa lepoarvoa (esim. agentin perus-uteliaisuus)
    /// ja säätää palautumisnopeutta (`decay_rate < 1` = tunne "tarttuu").
    fn apply_emotional_homeostasis(&mut self) {
        for dim in Dimension::ALL {
            let current = self.emotion.value(dim);
            // Lepotila kalibroinnista (neutraalisti 0.0).
            let baseline = self.calibration.baseline(dim);
            let deviation = current - baseline;
            if deviation.abs() > 0.01 {
                // decay_rate skaalaa palautumisnopeuden (neutraalisti 1.0).
                let rate = self.calibration.decay_rate(dim).max(0.0);
                let correction = deviation * HOMEOSTASIS_RATE * rate;
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

/// Kuvaa runtimen julkaisemat MCP-työkalukuvaukset LLM:lle tarjottaviksi
/// [`ToolDefinition`]:eiksi (tool-loop, Phase 1 keystone).
///
/// Vain **kelvolliset** määritelmät ([`ToolDefinition::validate`]) tarjotaan
/// mallille — viallinen nimi tai ei-objekti-skeema ohitetaan ja kirjataan
/// debug-tasolla, jottei yksi kelvoton taito kaada koko kutsua. `name`,
/// `description` ja `input_schema` (→ `function.parameters`) tulevat suoraan
/// kuvauksesta; vaadittu oikeus / luotettavuus ovat actions-kerroksen vastuulla
/// eivätkä kuulu LLM:lle tarjottavaan muotoon.
fn build_tool_definitions(descriptors: &[McpToolDescriptor]) -> Vec<ToolDefinition> {
    descriptors
        .iter()
        .filter_map(|d| {
            let def = ToolDefinition {
                name: d.name.clone(),
                description: d.description.clone(),
                input_schema: d.input_schema.clone(),
            };
            match def.validate() {
                Ok(()) => Some(def),
                Err(e) => {
                    debug!(tool = d.name.as_str(), error = %e, "tool loop: skipping invalid tool definition");
                    None
                }
            }
        })
        .collect()
}

/// Johtaa työkalukutsun tuloksesta mallille takaisin syötettävän tekstin
/// (tool-loop, Phase 1 keystone).
///
/// Käyttää tehtävän **redaktoitua todistepakettia** jos sellainen on syntynyt
/// ([`ActionRuntime::proof`]): todisteen `redacted_output` (salaisuudet
/// poistettu) sarjallistetaan JSON-tekstiksi, etuliitteenä lyhyt yhteenveto.
/// Jos todistetta ei ole (esim. tehtävä ei tuottanut sellaista), palautetaan
/// pelkkä tilakuvaus. Tuloste ei koskaan sisällä raakaa salaisuutta — todiste
/// on redaktoitu jo actions-putkessa.
fn tool_result_text(runtime: &ActionRuntime, submit: &familyclaw_actions::SubmitOutcome) -> String {
    if let Some(proof) = runtime.proof(submit.task_id) {
        let body = serde_json::to_string(&proof.redacted_output)
            .unwrap_or_else(|_| "{}".to_string());
        format!("status={:?}; {}; output={body}", submit.status, proof.output_summary)
    } else {
        format!("status={:?}; no proof produced", submit.status)
    }
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
        let mut agent = test_agent("agent_a", bus.clone()).with_default_governor();
        // Simuloi "pelokas" tila, jotta LLM EI suodattaisi
        // (governor_decide olisi Hesitate), mutta silti saamme testin
        // kattamaan EmotionPulse-polun. Annetaan tilalle neutraali.
        agent.emotion = EmotionState::neutral();
        // EmotionPulse sisarukselta → pitäisi palauttaa onnistunut turn
        // ilman kaatumista. (LLM:ää ei ole → thought_response = None, mutta
        // polku menee governor-suodatuksen läpi.)
        let outcome = agent
            .handle_turn(
                BeingId::new(),
                &BusMessage::emotion_pulse(EmotionState::neutral()),
            )
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

    /// FIX 1: ei-neutraali kalibrointi muuttaa agentin tunnetilan
    /// kehitystä — ja sitä kautta governorin [`ActionDecision`]:ia —
    /// verrattuna neutraaliin kalibrointiin. Tämä todistaa että
    /// `calibration.json` ei ole enää koristeellinen vaan vaikuttaa
    /// käytökseen.
    ///
    /// Mekanismi: governor lukee `self.emotion`-tilaa. Homeostaasi vetää
    /// tilan kohti kalibroinnin `baseline`-lepotilaa. Ei-neutraali kalibrointi
    /// (korkea Curiosity-baseline) pitää tilan korkealla, kun neutraali vetää
    /// sen kohti nollaa → eri governor-päätös samalla profiililla ja syötteellä.
    #[tokio::test]
    async fn non_neutral_calibration_changes_governor_decision_vs_neutral() {
        use familyclaw_emotion::{
            ActionDecision, Dimension, EmotionActionGovernor, GoverningProfile, NeutralCalibration,
            TableCalibration,
        };

        // Apuri: rakentaa agentin annetulla kalibroinnilla, ajaa N tekstivuoroa
        // (homeostaasin annetaan suppeta kohti kalibroinnin lepotilaa) ja
        // palauttaa governorin päätöksen + lopullisen Curiosity-arvon.
        // (Määritelty ennen lauseita: clippy::items_after_statements.)
        async fn decide_after_text_turns(
            calibration: Box<dyn EmotionCalibration + Send + Sync>,
            profile: &GoverningProfile,
            turns: usize,
        ) -> (ActionDecision, f32) {
            let bus = ResonanceBus::start(None).await.expect("bus");
            let mut agent = test_agent("agent_cal", bus.clone()).with_calibration(calibration);
            for _ in 0..turns {
                agent
                    .handle_turn(BeingId::new(), &BusMessage::text("hei sisarus"))
                    .await
                    .expect("turn");
            }
            let curiosity = agent.emotion().value(Dimension::Curiosity);
            let decision = EmotionActionGovernor::new(profile).decide(agent.emotion());
            bus.stop();
            (decision, curiosity)
        }

        // Yhteinen profiili molemmille: lievä warmth-kynnys, ei vaadi blendiä,
        // jotta yksittäinen korkea lämmin dimensio (Curiosity) riittää
        // nostamaan governorin EngageWarmly-tilaan.
        let profile = GoverningProfile::new("relaxed", 90.0, 50.0, 80.0, 1.0, false);

        // 1. NEUTRAALI kalibrointi: tekstikontakti nostaa Curiosityn +5.0/vuoro,
        //    homeostaasi vetää 10% kohti lepotilaa 0. Kiintopiste x=(x+5)*0.9 →
        //    Curiosity suppenee ~45:een, alle warmth-kynnyksen (50) → Reflect.
        let (neutral_decision, neutral_curiosity) =
            decide_after_text_turns(Box::new(NeutralCalibration), &profile, 80).await;

        // 2. EI-NEUTRAALI kalibrointi: korkea Curiosity-baseline (70).
        //    Homeostaasi vetää tilaa KOHTI 70:tä, ei nollaa. Kiintopiste
        //    nostaa Curiosityn kattoon (~100), reilusti yli warmth-kynnyksen
        //    (50) → EngageWarmly. ERI päätös samalla profiililla + syötteellä.
        let warm_cal = TableCalibration::new("warm_curious")
            .with_baseline(Dimension::Curiosity, 70.0)
            .with_sensitivity(Dimension::Curiosity, 1.0);
        let (warm_decision, warm_curiosity) =
            decide_after_text_turns(Box::new(warm_cal), &profile, 80).await;

        // Tunnetila kehittyi eri tavalla (todiste että kalibrointi vaikuttaa).
        assert!(
            warm_curiosity > neutral_curiosity + 50.0,
            "ei-neutraali baseline pitää Curiosityn korkealla \
             (warm={warm_curiosity}, neutral={neutral_curiosity})"
        );
        // Ja governorin päätös eroaa: neutraalilla Reflect, lämpimällä
        // EngageWarmly.
        assert_eq!(neutral_decision, ActionDecision::Reflect);
        assert_eq!(warm_decision, ActionDecision::EngageWarmly);
        assert_ne!(
            warm_decision, neutral_decision,
            "ei-neutraali kalibrointi muuttaa governorin päätöstä"
        );
    }

    /// FIX 1 (toinen mekanismi): kalibroinnin `sensitivity` skaalaa
    /// kontaktiärsykkeen voimakkuutta — sama tekstivuoro nostaa Curiosityä
    /// enemmän korkealla herkkyydellä kuin neutraalilla.
    #[tokio::test]
    async fn calibration_sensitivity_scales_text_stimulus() {
        use familyclaw_emotion::{Dimension, NeutralCalibration, TableCalibration};

        let bus = ResonanceBus::start(None).await.expect("bus");

        // Neutraali (sensitivity = 1.0): +5.0 stimulus, homeostaasi → 4.5.
        let mut neutral_agent =
            test_agent("agent_n", bus.clone()).with_calibration(Box::new(NeutralCalibration));
        neutral_agent
            .handle_turn(BeingId::new(), &BusMessage::text("kontakti"))
            .await
            .expect("turn");
        let neutral_curiosity = neutral_agent.emotion().value(Dimension::Curiosity);

        // Korkea herkkyys (3.0): +15.0 stimulus, homeostaasi → 13.5.
        let sensitive_cal =
            TableCalibration::new("sensitive").with_sensitivity(Dimension::Curiosity, 3.0);
        let mut sensitive_agent =
            test_agent("agent_s", bus.clone()).with_calibration(Box::new(sensitive_cal));
        sensitive_agent
            .handle_turn(BeingId::new(), &BusMessage::text("kontakti"))
            .await
            .expect("turn");
        let sensitive_curiosity = sensitive_agent.emotion().value(Dimension::Curiosity);

        assert!(
            sensitive_curiosity > neutral_curiosity,
            "korkea herkkyys nostaa Curiosityä enemmän \
             (sensitive={sensitive_curiosity}, neutral={neutral_curiosity})"
        );
        // Tarkat arvot: neutraali 4.5, herkkä 13.5 (3x stimulus).
        assert_eq!(neutral_curiosity, 4.5);
        assert_eq!(sensitive_curiosity, 13.5);

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

    // ---- 1B tool loop --------------------------------------------------------

    // ToolLoopConfig + ActionRuntime tulevat jo `use super::*`:n kautta
    // (ToolLoopConfig on tämän moduulin tyyppi; ActionRuntime importattu
    // agentti-moduulin yläosassa). LlmConfig on agentti-moduulissa yksityinen
    // `use`, joten se tuodaan tähän eksplisiittisesti.
    use crate::llm::LlmConfig;
    use std::sync::Arc as StdArc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Mutex as TokioMutex;

    /// Käynnistää **skriptatun fake-LLM:n**: OpenAI-yhteensopiva HTTP-endpoint,
    /// joka palauttaa annetut vastausrungot (JSON) järjestyksessä, yksi per
    /// pyyntö. Palauttaa base-URL:n (`http://127.0.0.1:PORT/v1`), jonka voi
    /// antaa [`LlmConfig`]:lle. Server elää kunnes kaikki rungot on käytetty.
    ///
    /// Tämä on sama raaka-TCP-kuvio kuin `llm.rs`:n timeout/empty-choices
    /// -testeissä — ei ulkoista mock-kirjastoa, ei verkkoa ulospäin.
    async fn spawn_scripted_llm(bodies: Vec<String>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind scripted llm");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            for body in bodies {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 4096];
                // Lue (ja hylkää) pyyntö; emme tarkista runkoa tässä testissä.
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}/v1")
    }

    /// OpenAI-vastausrunko jossa **pelkkä teksti** (ei työkalukutsuja) → silmukka
    /// pysähtyy.
    fn body_text(text: &str) -> String {
        serde_json::json!({
            "choices": [ { "message": { "content": text } } ]
        })
        .to_string()
    }

    /// OpenAI-vastausrunko jossa **yksi työkalukutsu**. `arguments` on JSON-arvo
    /// (cratimme deserialisoija odottaa litteää `{id,name,arguments}`-muotoa).
    fn body_tool_call(id: &str, name: &str, arguments: &serde_json::Value) -> String {
        serde_json::json!({
            "choices": [ {
                "message": {
                    "content": serde_json::Value::Null,
                    "tool_calls": [ { "id": id, "name": name, "arguments": arguments } ]
                }
            } ]
        })
        .to_string()
    }

    /// Rakentaa agentin skriptatulla LLM:llä (yksi endpoint, ei fallbackeja).
    fn agent_with_scripted_llm(name: &str, bus: BusHandle, api_base: &str) -> Agent {
        let config = AgentConfig::new(name, ModelConfig::new("scripted/model"));
        let soul = Soul::from_essence(format!("I am {name}."));
        let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
        let durable =
            DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
                .expect("durable ctx");
        let cfg = LlmConfig::new(api_base, "test-key", "scripted-model")
            .with_request_timeout_ms(2_000)
            .with_connect_timeout_ms(2_000);
        Agent::new(config, soul, memory, durable, bus, Some(cfg), None)
    }

    /// Read-only testitaito tool-loopia varten: kaiuttaa payloadin `q`-kentän
    /// takaisin tulosteeseen. Auto-run (ei hyväksyntää), jotta silmukka voi
    /// syöttää tuloksen takaisin malliin.
    #[derive(Debug, Clone, Default)]
    struct LoopEchoSkill;

    /// Testitaidon kiinteä tunniste (deterministinen).
    const LOOP_ECHO_UUID: uuid::Uuid = uuid::uuid!("11111111-2222-4333-8444-555555555555");

    #[async_trait::async_trait]
    impl familyclaw_actions::ActionExecutor for LoopEchoSkill {
        async fn execute(
            &self,
            request: familyclaw_actions::ActionRequest,
        ) -> familyclaw_actions::Result<familyclaw_actions::ActionResult> {
            let q = request
                .payload
                .get("q")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Ok(familyclaw_actions::ActionResult::success(
                "echoed loop input",
                serde_json::json!({ "echoed": q }),
                request.now,
            ))
        }
    }

    impl familyclaw_actions::Skill for LoopEchoSkill {
        fn manifest(&self) -> familyclaw_actions::manifest::SkillManifest {
            familyclaw_actions::manifest::SkillManifest {
                id: familyclaw_actions::SkillId::from_uuid(LOOP_ECHO_UUID),
                name: "loop_echo".to_string(),
                version: "1.0.0".to_string(),
                description: "Kaiuttaa payloadin q-kentän (vain luku, testikäyttö).".to_string(),
                permissions: vec![familyclaw_actions::policy::SkillPermission::ReadFiles],
                risk: familyclaw_actions::policy::ActionRisk::ReadOnly,
                approval_policy: familyclaw_actions::policy::ApprovalPolicy::AutoIfReadOnly,
                input_hint: None,
                output_hint: None,
                input_schema: familyclaw_actions::manifest::default_input_schema(),
            }
        }
    }

    /// Rakentaa jaetun runtimen, johon on rekisteröity `loop_echo`-testitaito.
    fn echo_runtime() -> StdArc<TokioMutex<ActionRuntime>> {
        let mut rt = ActionRuntime::new();
        rt.register_skill(LoopEchoSkill).expect("register loop_echo");
        StdArc::new(TokioMutex::new(rt))
    }

    /// Hyväksyntää vaativa testitaito tool-loopia varten: mallintaa ulkoisesti
    /// kirjoittavan (`WriteExternal`) toiminnon, joka EI ole auto-ajettava ja
    /// joka pysähtyy odottamaan ihmisen hyväksyntää
    /// ([`SubmitOutcome::pending_approval`] = `Some`). Käytetään todistamaan
    /// ettei hyväksyntää odottava ohjaustila vuoda käyttäjälle.
    #[derive(Debug, Clone, Default)]
    struct ApprovalSkill;

    /// Hyväksyntätaidon kiinteä tunniste (deterministinen).
    const APPROVAL_UUID: uuid::Uuid = uuid::uuid!("99999999-2222-4333-8444-555555555555");

    #[async_trait::async_trait]
    impl familyclaw_actions::ActionExecutor for ApprovalSkill {
        async fn execute(
            &self,
            request: familyclaw_actions::ActionRequest,
        ) -> familyclaw_actions::Result<familyclaw_actions::ActionResult> {
            // Ei pitäisi koskaan suoriutua ilman hyväksyntää tässä testissä,
            // mutta paluuarvo tarvitaan tyyppisopimukseen.
            Ok(familyclaw_actions::ActionResult::success(
                "approval-gated action executed",
                serde_json::json!({ "ok": true }),
                request.now,
            ))
        }
    }

    impl familyclaw_actions::Skill for ApprovalSkill {
        fn manifest(&self) -> familyclaw_actions::manifest::SkillManifest {
            familyclaw_actions::manifest::SkillManifest {
                id: familyclaw_actions::SkillId::from_uuid(APPROVAL_UUID),
                name: "approval_skill".to_string(),
                version: "1.0.0".to_string(),
                description: "Ulkoisesti kirjoittava toiminto (vaatii ihmisen hyväksynnän, testikäyttö)."
                    .to_string(),
                permissions: vec![familyclaw_actions::policy::SkillPermission::WriteExternal],
                risk: familyclaw_actions::policy::ActionRisk::WriteExternal,
                approval_policy: familyclaw_actions::policy::ApprovalPolicy::RequireApproval,
                input_hint: None,
                output_hint: None,
                input_schema: familyclaw_actions::manifest::default_input_schema(),
            }
        }
    }

    /// Rakentaa jaetun runtimen, johon on rekisteröity hyväksyntää vaativa
    /// `approval_skill`-testitaito.
    fn approval_runtime() -> StdArc<TokioMutex<ActionRuntime>> {
        let mut rt = ActionRuntime::new();
        rt.register_skill(ApprovalSkill)
            .expect("register approval_skill");
        StdArc::new(TokioMutex::new(rt))
    }

    /// (a) `actions = None` säilyttää yhden kerran -käytöksen: yksi LLM-kutsu,
    /// ei työkaluja, palautuu mallin teksti sellaisenaan.
    #[tokio::test]
    async fn tool_loop_none_keeps_one_shot() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let api = spawn_scripted_llm(vec![body_text("yksi vastaus")]).await;
        let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api);
        assert!(!agent.has_actions(), "oletus: ei toimintoja → yhden kerran");

        let out = agent
            .think(&BusMessage::text("hei"))
            .await
            .expect("llm present")
            .expect("one-shot ok");
        assert_eq!(out, "yksi vastaus");
        bus.stop();
    }

    /// (b) Tool-loop pysähtyy heti kun malli vastaa ilman työkalukutsuja
    /// (vaikka työkaluja olisi tarjolla).
    #[tokio::test]
    async fn tool_loop_stops_on_no_tool_calls() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let api = spawn_scripted_llm(vec![body_text("ei työkaluja tarvita")]).await;
        let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api).with_actions(echo_runtime());
        assert!(agent.has_actions());

        let out = agent
            .think(&BusMessage::text("kysymys"))
            .await
            .expect("llm present")
            .expect("loop ok");
        assert_eq!(out, "ei työkaluja tarvita");
        bus.stop();
    }

    /// (c) Työkalukutsu dispatchataan ja tulos syötetään takaisin: ensimmäinen
    /// vastaus pyytää `loop_echo`-työkalua, toinen (tuloksen nähtyään) vastaa
    /// tekstillä → silmukka pysähtyy lopulliseen tekstiin.
    #[tokio::test]
    async fn tool_loop_dispatches_tool_and_feeds_result_back() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let api = spawn_scripted_llm(vec![
            body_tool_call("call_1", "loop_echo", &serde_json::json!({ "q": "ping" })),
            body_text("työkalu vastasi, valmis"),
        ])
        .await;
        let agent =
            agent_with_scripted_llm("agent_a", bus.clone(), &api).with_actions(echo_runtime());

        let out = agent
            .think(&BusMessage::text("aja työkalu"))
            .await
            .expect("llm present")
            .expect("loop ok");
        // Toinen kierros pysähtyi lopulliseen tekstiin (työkalun tulos
        // syötettiin takaisin malliin ennen tätä).
        assert_eq!(out, "työkalu vastasi, valmis");
        bus.stop();
    }

    /// (d) Tuntematon työkalu → virhe-`tool_result` syötetään takaisin, silmukka
    /// JATKAA (ei keskeydy). Seuraava vastaus on tekstiä → pysähdys.
    #[tokio::test]
    async fn tool_loop_unknown_tool_does_not_abort() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let api = spawn_scripted_llm(vec![
            body_tool_call("call_x", "does_not_exist", &serde_json::json!({})),
            body_text("ok, jatketaan ilman sitä työkalua"),
        ])
        .await;
        let agent =
            agent_with_scripted_llm("agent_a", bus.clone(), &api).with_actions(echo_runtime());

        let out = agent
            .think(&BusMessage::text("kokeile tuntematonta"))
            .await
            .expect("llm present")
            .expect("loop continues past unknown tool");
        assert_eq!(out, "ok, jatketaan ilman sitä työkalua");
        bus.stop();
    }

    /// (e) Kierrosraja sitoo silmukan: jos malli pyytää AINA työkalua eikä
    /// koskaan vastaa tekstillä, silmukka pysähtyy `max_iterations`-rajaan
    /// EIKÄ jää ikuiseen kiertoon. Skriptataan tarkalleen `max` työkalukutsua
    /// (server ei vastaa enempää → jos silmukka ylittäisi rajan, seuraava
    /// LLM-kutsu hyytyisi timeoutiin; raja estää sen).
    ///
    /// **Käyttäjäraja:** kun raja täyttyy ilman vastausta, `think()` palauttaa
    /// `None` — sisäistä max-iter-merkkiä EI reititetä käyttäjälle. Aiempi
    /// toteutus vuoti `"[tool loop stopped: ...]"`-merkkijonon sanatarkasti
    /// reply-putken läpi; tämä testi vartioi ettei näin enää tapahdu.
    #[tokio::test]
    async fn tool_loop_max_iterations_does_not_leak_marker_to_user() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let max = 3u32;
        // Tasan `max` työkalukutsu-vastausta — yksi per kierros. Silmukka EI saa
        // pyytää (max+1):ttä vastausta serveriltä.
        let bodies: Vec<String> = (0..max)
            .map(|i| {
                body_tool_call(
                    &format!("call_{i}"),
                    "loop_echo",
                    &serde_json::json!({ "q": i }),
                )
            })
            .collect();
        let api = spawn_scripted_llm(bodies).await;
        let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
            .with_actions(echo_runtime())
            .with_tool_loop(ToolLoopConfig { max_iterations: max });

        // Silmukka pysähtyy rajaan ilman paniikkia/hangia. Koska malli ei
        // koskaan tuottanut tekstiä, käyttäjälle EI synny vastausta: `think`
        // palauttaa `None` (sisäinen MaxIterations-ohjaustila suodatettu),
        // EIKÄ raakaa max-iter-merkkijonoa vuoda käyttäjärajan yli.
        let out = agent.think(&BusMessage::text("ikuinen työkalupyyntö")).await;
        assert!(
            matches!(out, Some(Ok(ref s)) if !s.contains("tool loop stopped")) || out.is_none(),
            "max-iter EI saa vuotaa sisäistä merkkiä käyttäjälle, sai: {out:?}"
        );
        // Tarkka odotus: ei tekstiä → None (ei käyttäjäreplyä tällä vuorolla).
        assert!(
            out.is_none(),
            "ilman mallin tekstiä max-iter ei tuota käyttäjävastausta, sai: {out:?}"
        );
        bus.stop();
    }

    /// (f) **Hyväksyntää vaativa työkalu EI vuoda sisäistä merkkiä käyttäjälle.**
    /// Kun malli kutsuu hyväksyntää vaativaa työkalua, suoritus jää odottamaan
    /// ihmisen lupaa. Aiempi toteutus palautti `"[awaiting approval: ...
    /// (approval_id=...)]"` tavallisena onnistumis-merkkijonona, joka reititettiin
    /// sanatarkasti — raaka `approval_id` mukaan lukien — käyttäjälle. Korjattu:
    /// `think()` palauttaa `None` (sisäinen `AwaitingApproval`-ohjaustila), eikä
    /// mitään `approval`-tekstiä koskaan päädy käyttäjärajan yli.
    #[tokio::test]
    async fn tool_loop_awaiting_approval_does_not_leak_marker_to_user() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        // Yksi vastaus: malli kutsuu hyväksyntää vaativaa työkalua.
        let api = spawn_scripted_llm(vec![body_tool_call(
            "call_approve",
            "approval_skill",
            &serde_json::json!({ "q": "do-it" }),
        )])
        .await;
        let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
            .with_actions(approval_runtime());

        let out = agent.think(&BusMessage::text("aja hyväksyntä-työkalu")).await;
        // EI käyttäjävastausta: hyväksyntä on operaattorin tila, ei reply.
        assert!(
            out.is_none(),
            "hyväksyntää odottava työkalu ei tuota käyttäjävastausta, sai: {out:?}"
        );
        // Varmistus: jos jokin tulevaisuuden muutos palauttaisi tekstin, se ei
        // saa sisältää sisäistä merkkiä eikä raakaa approval-tunnistetta.
        if let Some(Ok(text)) = out {
            assert!(
                !text.contains("awaiting approval") && !text.contains("approval_id"),
                "approval-merkki EI saa vuotaa käyttäjälle, sai: {text}"
            );
        }
        bus.stop();
    }

    /// `with_tool_loop` säätää rajan ja `tool_loop()` lukee sen; oletus on
    /// [`ToolLoopConfig::DEFAULT_MAX_ITERATIONS`].
    #[tokio::test]
    async fn tool_loop_config_default_and_override() {
        let bus = ResonanceBus::start(None).await.expect("bus");
        let agent = test_agent("agent_a", bus.clone());
        assert_eq!(
            agent.tool_loop().max_iterations,
            ToolLoopConfig::DEFAULT_MAX_ITERATIONS
        );
        let tuned = agent.with_tool_loop(ToolLoopConfig { max_iterations: 2 });
        assert_eq!(tuned.tool_loop().max_iterations, 2);
        bus.stop();
    }
}
