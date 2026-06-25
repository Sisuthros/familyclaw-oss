//! Moniagenttiorkesterointi: DAG-pohjainen työnkulkumoottori (director→worker).
//!
//! Tämä moduuli rakentuu **pelkästään** sillan julkisen rajapinnan
//! ([`crate::bridge::FamilyBridge`]) päälle: agenttirekisteri, tehtävätaulu ja
//! tapahtumaväylä. Se mallintaa työnkulun suunnattuna syklittömänä verkkona
//! ([`OrchestrationPlan`]), jossa jokainen solmu ([`TaskNode`]) muuttuu
//! ajossa konkreettiseksi [`crate::task::Task`]:ksi tehtävätaululla.
//!
//! ## Suunnitteluperiaatteet
//! - **Vain lailliset tilasiirtymät.** Orkesteri ohjaa tehtäviä ainoastaan
//!   jäädytetyn [`crate::task::TaskStatus`]-tilakoneen sallimia siirtymiä pitkin
//!   (`Pending → Active → Done`), jotta durable-replay pysyy ehjänä.
//! - **Determinismi.** Kaikki ajalliset päätökset (liveness) ottavat `now`-
//!   parametrin; samasta suunnitelmasta ja samasta `now`-arvosta seuraa aina
//!   sama työjärjestys ja sama työntekijävalinta. Järjestelmäkelloa ei lueta.
//! - **Ei ydintyyppien laajennusta.** Koordinointitapahtumat julkaistaan
//!   [`crate::event::EventKind::Custom`]-muodossa etuliitteellä `orchestration.`.
//! - **Rajattu sub-delegointi.** Rekursiivinen alityönkulkujen ajo on katkaistu
//!   syvyysbudjettiin (vrt. Hermesin iteraatiobudjetti).
//!
//! ## Esimerkki
//! ```
//! use familyclaw_bridge::{
//!     AgentInfo, AgentRole, FamilyBridge, HostKind, NodeId, OrchestrationPlan,
//!     Orchestrator, TaskNode, TaskStatus,
//! };
//! use familyclaw_core::ids::AgentId;
//! use familyclaw_core::time;
//!
//! # async fn run() -> familyclaw_core::Result<()> {
//! let bridge = FamilyBridge::new();
//! let worker = AgentInfo::new(AgentId::new(), "w", AgentRole::Executor, HostKind::Local);
//! let wid = worker.id;
//! bridge.register_agent(worker).await?;
//! let now = time::now();
//! bridge.heartbeat(wid, now).await?; // tee työntekijästä online
//!
//! let plan = OrchestrationPlan::new("demo", vec![
//!     TaskNode::new("a", "step a", "do a"),
//! ]);
//! let orch = Orchestrator::new(bridge);
//! let report = orch.run(&plan, now).await?;
//! assert_eq!(report.completed.len(), 1);
//! # Ok(())
//! # }
//! ```

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use familyclaw_core::ids::AgentId;
use familyclaw_core::time::Timestamp;
use familyclaw_core::{FamilyClawError, Result};

use crate::agent::{AgentRole, Liveness};
use crate::bridge::FamilyBridge;
use crate::contract::{Capability, ContractBoard, Deliverable};
use crate::event::{Event, EventKind};
use crate::executor::{OrchestratedTurn, TurnExecutor};
use crate::task::{TaskId, TaskStatus};

/// Tapahtuma jonka orkesteri julkaisee kun solmun tehtävä on osoitettu
/// työntekijälle ja aktivoitu.
pub const STEP_ASSIGNED: &str = "orchestration.step_assigned";

/// Tapahtuma jonka orkesteri julkaisee kun koko työnkulku on valmis (kaikki
/// solmut tilassa [`TaskStatus::Done`]).
pub const WORKFLOW_DONE: &str = "orchestration.workflow_done";

/// Tapahtuma jonka orkesteri julkaisee kun solmun vuoro **epäonnistui**:
/// suorittaja palautti virheen tai toimite rikkoi solmun kyvyn sopimuksen
/// (tulosskeema/jälkiehto). Solmun tehtävä jätetään ei-`Done`-tilaan eikä sen
/// jälkeläisiä etenetä — [`TaskStatus`]-tilakoneessa ei ole `Failed`-arvoa,
/// joten epäonnistuminen ilmaistaan tällä [`EventKind::Custom`]-tapahtumalla.
pub const STEP_FAILED: &str = "orchestration.step_failed";

/// Rekursiivisen sub-delegoinnin syvyyskatto (vrt. iteraatiobudjetti).
///
/// Tämän ylittävä [`Orchestrator::run_nested`]-kutsu palauttaa virheen sen
/// sijaan että ajaisi rajattomasti syvemmälle.
pub const MAX_DELEGATION_DEPTH: usize = 4;

/// Työnkulun yksittäisen solmun vakaa tunniste (ihmisluettava merkkijono).
///
/// Toisin kuin UUID-pohjaiset [`crate::task::TaskId`]-arvot, `NodeId` on
/// suunnittelijan antama nimi (esim. `"build"`, `"test"`), jolloin
/// riippuvuudet kirjoitetaan luettavasti ja topologinen järjestys on vakaa.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl NodeId {
    /// Rakentaa tunnisteen mistä tahansa merkkijonoksi muunnettavasta arvosta.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Palauttaa tunnisteen merkkijonona.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for NodeId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for NodeId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Yksittäinen työnkulun solmu: yksi delegoitava työvaihe.
///
/// Solmu kuvaa *mitä* tehdään ja *kenelle se sopii* (rooli + kyvykkyydet),
/// sekä *minkä jälkeen* se voi alkaa ([`deps`](Self::deps)). Ajossa solmu
/// muuttuu yhdeksi tehtäväksi tehtävätaululla.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNode {
    /// Solmun vakaa tunniste työnkulun sisällä (oltava uniikki).
    pub id: NodeId,

    /// Lyhyt otsikko (tulee tehtävän otsikoksi).
    pub title: String,

    /// Vapaamuotoinen kuvaus työvaiheesta.
    #[serde(default)]
    pub description: String,

    /// Vaadittu rooli työntekijälle, tai `None` jos mikä tahansa rooli kelpaa.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_role: Option<AgentRole>,

    /// Vaaditut kyvykkyydet: työntekijän kyvykkyysjoukon on oltava näiden
    /// ylijoukko (superset).
    #[serde(default)]
    pub required_capabilities: Vec<String>,

    /// Solmut joiden on oltava valmiita ennen kuin tämä voi alkaa.
    #[serde(default)]
    pub deps: Vec<NodeId>,

    /// Kiinnitetty työntekijä: jos asetettu, valinta ohitetaan ja tehtävä
    /// osoitetaan suoraan tälle agentille (jos se on online).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_assignee: Option<AgentId>,

    /// Valinnainen kyky/sopimus jota solmun toimite todennetaan vasten
    /// suoritussauman ([`crate::executor::TurnExecutor`]) jälkeen. Jos asetettu,
    /// [`Orchestrator::run_with`] ajaa toimitteen
    /// [`crate::contract::ContractBoard::fulfill`]-todennuksen läpi (tulosskeema
    /// ja jälkiehdot) **ennen** kuin solmu siirretään `Done`-tilaan; rikkomus
    /// merkitsee solmun epäonnistuneeksi eikä jälkeläisiä eteneä.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<Capability>,
}

impl TaskNode {
    /// Rakentaa solmun tunnisteella, otsikolla ja kuvauksella ilman rajoitteita.
    pub fn new(
        id: impl Into<NodeId>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: description.into(),
            required_role: None,
            required_capabilities: Vec::new(),
            deps: Vec::new(),
            pinned_assignee: None,
            capability: None,
        }
    }

    /// Asettaa vaaditun roolin (builder-tyyli).
    #[must_use]
    pub fn with_role(mut self, role: AgentRole) -> Self {
        self.required_role = Some(role);
        self
    }

    /// Asettaa vaaditut kyvykkyydet (builder-tyyli).
    #[must_use]
    pub fn with_capabilities<I, S>(mut self, caps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.required_capabilities = caps.into_iter().map(Into::into).collect();
        self
    }

    /// Asettaa riippuvuudet (builder-tyyli).
    #[must_use]
    pub fn with_deps<I, N>(mut self, deps: I) -> Self
    where
        I: IntoIterator<Item = N>,
        N: Into<NodeId>,
    {
        self.deps = deps.into_iter().map(Into::into).collect();
        self
    }

    /// Kiinnittää työntekijän (builder-tyyli).
    #[must_use]
    pub fn with_pinned_assignee(mut self, agent: AgentId) -> Self {
        self.pinned_assignee = Some(agent);
        self
    }

    /// Liittää solmuun todennettavan kyvyn/sopimuksen (builder-tyyli).
    ///
    /// Kun solmulla on kyky, [`Orchestrator::run_with`] ajaa suoritussauman
    /// tuottaman toimitteen [`crate::contract::ContractBoard::fulfill`]-
    /// todennuksen läpi ennen `Done`-siirtymää.
    #[must_use]
    pub fn with_capability(mut self, capability: Capability) -> Self {
        self.capability = Some(capability);
        self
    }
}

/// Suunnattu syklitön työnkulkukuvaus (DAG).
///
/// Suunnitelma kootaan solmuista, joiden väliset riippuvuudet
/// ([`TaskNode::deps`]) muodostavat verkon. [`validate`](Self::validate)
/// varmistaa että verkko on kelvollinen (ei syklejä, ei roikkuvia
/// riippuvuuksia, ei kaksoistunnisteita) ja [`topo_order`](Self::topo_order)
/// palauttaa deterministisen suoritusjärjestyksen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationPlan {
    /// Suunnitelman ihmisluettava tunniste.
    pub id: String,

    /// Työnkulun solmut.
    pub nodes: Vec<TaskNode>,
}

impl OrchestrationPlan {
    /// Rakentaa suunnitelman tunnisteella ja solmuilla (validoimatta).
    pub fn new(id: impl Into<String>, nodes: Vec<TaskNode>) -> Self {
        Self {
            id: id.into(),
            nodes,
        }
    }

    /// Hakee solmun tunnisteen perusteella.
    #[must_use]
    pub fn node(&self, id: &NodeId) -> Option<&TaskNode> {
        self.nodes.iter().find(|n| &n.id == id)
    }

    /// Validoi suunnitelman rakenteen.
    ///
    /// Hylkää:
    /// - **kaksoistunnisteen** (sama [`NodeId`] kahdesti),
    /// - **roikkuvan riippuvuuden** (dep osoittaa tuntemattomaan solmuun),
    /// - **syklin** (verkossa on kehä — topologinen järjestys ei onnistu).
    ///
    /// # Errors
    /// [`FamilyClawError::InvalidInput`] kuvaavalla viestillä jos jokin yllä
    /// mainituista ehdoista rikkoutuu.
    pub fn validate(&self) -> Result<()> {
        // 1) Kaksoistunnisteet.
        let mut seen: HashSet<&NodeId> = HashSet::with_capacity(self.nodes.len());
        for node in &self.nodes {
            if !seen.insert(&node.id) {
                return Err(FamilyClawError::invalid_input(format!(
                    "duplicate node id: {}",
                    node.id
                )));
            }
        }

        // 2) Roikkuvat riippuvuudet + itseviittaus.
        for node in &self.nodes {
            for dep in &node.deps {
                if !seen.contains(dep) {
                    return Err(FamilyClawError::invalid_input(format!(
                        "node {} depends on unknown node {}",
                        node.id, dep
                    )));
                }
                if dep == &node.id {
                    return Err(FamilyClawError::invalid_input(format!(
                        "node {} depends on itself",
                        node.id
                    )));
                }
            }
        }

        // 3) Syklit: topo-sort onnistuu vain syklittömälle verkolle.
        self.topo_order()?;
        Ok(())
    }

    /// Palauttaa deterministisen topologisen suoritusjärjestyksen.
    ///
    /// Käyttää Kahnin algoritmia, jossa tasapelit (useita yhtä aikaa
    /// suoritettavissa olevia solmuja) ratkaistaan [`NodeId`]:n mukaan
    /// nousevasti — näin järjestys on sama joka ajolla.
    ///
    /// # Errors
    /// - [`FamilyClawError::InvalidInput`] jos jokin riippuvuus osoittaa
    ///   tuntemattomaan solmuun (roikkuva dep).
    /// - [`FamilyClawError::InvalidInput`] jos verkossa on sykli.
    pub fn topo_order(&self) -> Result<Vec<NodeId>> {
        let index: HashMap<&NodeId, &TaskNode> = self.nodes.iter().map(|n| (&n.id, n)).collect();

        // Lähtevien kaarien (dep → riippuva) muodostus + sisääntuloasteet.
        let mut in_degree: HashMap<&NodeId, usize> =
            self.nodes.iter().map(|n| (&n.id, 0usize)).collect();
        let mut dependents: HashMap<&NodeId, Vec<&NodeId>> =
            self.nodes.iter().map(|n| (&n.id, Vec::new())).collect();

        for node in &self.nodes {
            for dep in &node.deps {
                if !index.contains_key(dep) {
                    return Err(FamilyClawError::invalid_input(format!(
                        "node {} depends on unknown node {}",
                        node.id, dep
                    )));
                }
                // dep → node: node:n sisääntuloaste kasvaa.
                if let Some(d) = in_degree.get_mut(&node.id) {
                    *d += 1;
                }
                if let Some(list) = dependents.get_mut(dep) {
                    list.push(&node.id);
                }
            }
        }

        // Kahn: ota aina pienin valmis solmu (deterministinen tasapelin ratko).
        let mut ready: Vec<&NodeId> = in_degree
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(&id, _)| id)
            .collect();
        ready.sort();
        let mut queue: VecDeque<&NodeId> = ready.into_iter().collect();

        let mut order: Vec<NodeId> = Vec::with_capacity(self.nodes.len());
        while let Some(id) = queue.pop_front() {
            order.push(id.clone());
            let mut newly_ready: Vec<&NodeId> = Vec::new();
            if let Some(children) = dependents.get(id) {
                for child in children {
                    if let Some(d) = in_degree.get_mut(child) {
                        *d -= 1;
                        if *d == 0 {
                            newly_ready.push(child);
                        }
                    }
                }
            }
            // Lisää valmistuneet pienin-ensin ja pidä jono lajiteltuna jotta
            // järjestys on täysin deterministinen.
            if !newly_ready.is_empty() {
                let mut rest: Vec<&NodeId> = queue.drain(..).collect();
                rest.extend(newly_ready);
                rest.sort();
                queue = rest.into_iter().collect();
            }
        }

        if order.len() != self.nodes.len() {
            return Err(FamilyClawError::invalid_input(
                "orchestration plan contains a cycle",
            ));
        }
        Ok(order)
    }
}

/// Yhteenveto yhden työntekijävalinnan ehdokkaasta (sisäinen apurakenne).
struct Candidate {
    id: AgentId,
    in_flight: usize,
}

/// Yhden solmun ajon lopputulos (sisäinen).
enum NodeOutcome {
    /// Solmu valmistui: tehtävä on `Done`.
    Completed(TaskId),
    /// Solmu epäonnistui: tehtävä jäi ei-`Done`-tilaan, haara pysähtyy.
    Failed,
}

/// DAG-työnkulkumoottori joka ohjaa sillan tehtävätaulua.
///
/// `Orchestrator` ei omista omaa tilaa — se kuljettaa jaettua
/// [`FamilyBridge`]-julkisivua ja on siksi `Clone`. Kaikki ajalliset
/// päätökset (liveness) ottavat `now`-parametrin determinismin vuoksi.
#[derive(Debug, Clone)]
pub struct Orchestrator {
    bridge: FamilyBridge,
}

/// Raportti yhden suunnitelman ajosta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    /// Suunnitelman tunniste.
    pub plan_id: String,

    /// Valmistuneet solmut ja niiden tehtävätunnisteet topologisessa
    /// järjestyksessä.
    pub completed: Vec<(NodeId, TaskId)>,
}

impl Orchestrator {
    /// Rakentaa orkesterin annetun sillan ympärille.
    #[must_use]
    pub fn new(bridge: FamilyBridge) -> Self {
        Self { bridge }
    }

    /// Pääsy taustalla olevaan siltaan.
    #[must_use]
    pub fn bridge(&self) -> &FamilyBridge {
        &self.bridge
    }

    /// Valitsee parhaan online-työntekijän annetuilla rajoitteilla hetkellä
    /// `now`.
    ///
    /// Valintasäännöt:
    /// 1. Rooli täsmää (jos `required_role` annettu).
    /// 2. Kyvykkyydet ovat ylijoukko: agentin kyvyt sisältävät kaikki
    ///    vaaditut kyvyt.
    /// 3. Agentti on [`Liveness::Online`] hetkellä `now`.
    ///
    /// Tasapeli ratkaistaan deterministisesti: ensin vähiten keskeneräisiä
    /// (ei-terminaalisia) tehtäviä, sitten pienin [`AgentId`].
    ///
    /// Palauttaa `None` jos kukaan ei täytä ehtoja.
    pub async fn select_worker(
        &self,
        required_role: Option<AgentRole>,
        required_caps: &[String],
        now: Timestamp,
    ) -> Option<AgentId> {
        let registry = self.bridge.registry();
        let board = self.bridge.board();
        let agents = registry.list().await;

        let mut candidates: Vec<Candidate> = Vec::new();
        for info in agents {
            // 1) Rooli.
            if let Some(role) = required_role {
                if info.role != role {
                    continue;
                }
            }
            // 2) Kyvykkyydet (superset).
            let has_all = required_caps
                .iter()
                .all(|need| info.capabilities.iter().any(|have| have == need));
            if !has_all {
                continue;
            }
            // 3) Liveness.
            match registry.liveness_at(info.id, now).await {
                Ok(Liveness::Online) => {}
                _ => continue,
            }

            let in_flight = board
                .list_for_assignee(info.id)
                .await
                .into_iter()
                .filter(|t| !t.status.is_terminal())
                .count();
            candidates.push(Candidate {
                id: info.id,
                in_flight,
            });
        }

        candidates
            .into_iter()
            .min_by(|a, b| a.in_flight.cmp(&b.in_flight).then(a.id.cmp(&b.id)))
            .map(|c| c.id)
    }

    /// Ajaa työnkulun loppuun ohjaten jäädytettyä tehtävätaulua.
    ///
    /// Eteneminen: validoi suunnitelma, käy solmut läpi topologisessa
    /// järjestyksessä, ja jokaiselle solmulle (jonka kaikki riippuvuudet ovat
    /// [`TaskStatus::Done`]) luo tehtävän, valitsee työntekijän, asettaa
    /// `Pending → Active → Done`. Koordinointitapahtumat julkaistaan väylälle.
    ///
    /// Tämä on **synkroninen, in-process** ajuri: se simuloi työntekijän
    /// työn valmistumisen (siirtää tehtävän `Done`-tilaan) heti osoituksen
    /// jälkeen, koska varsinainen LLM-/kuljetuskerros kytketään adapterilla
    /// myöhemmin. Tärkeää on että **vain laillisia tilasiirtymiä** käytetään.
    ///
    /// # Errors
    /// - [`FamilyClawError::InvalidInput`] jos suunnitelma on kelvoton
    ///   (sykli/roikkuva dep/kaksoistunniste).
    /// - [`FamilyClawError::NotFound`] jos kiinnitettyä työntekijää ei löydy
    ///   tai sopivaa työntekijää ei ole (`NotFound` solmun nimellä).
    /// - Välittää tehtävätaulun siirtymävirheet.
    pub async fn run(&self, plan: &OrchestrationPlan, now: Timestamp) -> Result<RunReport> {
        self.run_nested(plan, now, 0).await
    }

    /// Kuten [`run`](Self::run), mutta seuraa rekursiivista delegointisyvyyttä.
    ///
    /// Sub-työnkulut (esim. solmu joka itse delegoi alityönkulun) kutsuvat
    /// tätä kasvavalla `depth`-arvolla. Kun `depth` ylittää
    /// [`MAX_DELEGATION_DEPTH`]:n, ajo katkaistaan virheellä budjetin
    /// ylityksen estämiseksi.
    ///
    /// Tämä on takautuvasti yhteensopiva sisäänkäynti: se delegoi
    /// [`run_nested_with`](Self::run_nested_with):lle hermeettisellä
    /// [`MockTurnExecutor`](crate::executor::MockTurnExecutor)-suorittajalla,
    /// joten simuloitu in-process-valmistuminen säilyy bittiyhteensopivana.
    ///
    /// # Errors
    /// Kuten [`run`](Self::run), sekä [`FamilyClawError::InvalidInput`] jos
    /// `depth > MAX_DELEGATION_DEPTH`.
    pub async fn run_nested(
        &self,
        plan: &OrchestrationPlan,
        now: Timestamp,
        depth: usize,
    ) -> Result<RunReport> {
        let executor = crate::executor::MockTurnExecutor::default();
        self.run_nested_with(plan, now, depth, &executor).await
    }

    /// Ajaa työnkulun loppuun reitittäen jokaisen solmun vuoron annetun
    /// [`TurnExecutor`]-sauman läpi.
    ///
    /// Toisin kuin [`run`](Self::run) (joka simuloi valmistumisen sisäisesti
    /// [`MockTurnExecutor`](crate::executor::MockTurnExecutor):lla), tämä antaa
    /// kutsujan kytkeä **oikean** suorittajan (esim. LLM-/kuljetuskerros) ilman
    /// että orkesteri muuttuu. Jokaiselle ajovalmiille solmulle:
    ///
    /// 1. rakennetaan [`OrchestratedTurn`] (solmun otsikko/kuvaus + valittu
    ///    suorittaja + injektoitu `now`),
    /// 2. kutsutaan [`TurnExecutor::execute`] joka palauttaa toimitteen,
    /// 3. jos solmulla on kyky/sopimus ([`TaskNode::capability`]), toimite
    ///    ajetaan [`ContractBoard::fulfill`]-todennuksen läpi (tulosskeema +
    ///    jälkiehdot) **ennen** `Done`-siirtymää,
    /// 4. hyväksyttävä toimite siirtää tehtävän `Active → Done`; muutoin solmu
    ///    merkitään epäonnistuneeksi (tehtävä jää ei-`Done`-tilaan,
    ///    [`STEP_FAILED`] julkaistaan) eikä sen jälkeläisiä eteneä.
    ///
    /// Determinismi säilyy: kelloa ei lueta, vaan `now` injektoidaan. Suorittajan
    /// palauttama [`Err`] (esim. kuljetusvirhe) ei jää roikkumaan: solmu merkitään
    /// epäonnistuneeksi ja sen haara pysähtyy.
    ///
    /// # Errors
    /// Sama virhejoukko kuin [`run`](Self::run): kelvoton suunnitelma, ei
    /// kelvollista työntekijää, offline kiinnitetty työntekijä, syvyysbudjetin
    /// ylitys tai tehtävätaulun siirtymävirhe.
    pub async fn run_with(
        &self,
        plan: &OrchestrationPlan,
        now: Timestamp,
        executor: &dyn TurnExecutor,
    ) -> Result<RunReport> {
        self.run_nested_with(plan, now, 0, executor).await
    }

    /// [`run_with`](Self::run_with) + rekursiivinen delegointisyvyys.
    ///
    /// Tämä on **varsinainen** orkesterointisilmukka jonka kautta
    /// [`run`](Self::run), [`run_nested`](Self::run_nested) ja
    /// [`run_with`](Self::run_with) kaikki kulkevat.
    ///
    /// # Errors
    /// Kuten [`run_with`](Self::run_with), sekä [`FamilyClawError::InvalidInput`]
    /// jos `depth > MAX_DELEGATION_DEPTH`.
    pub async fn run_nested_with(
        &self,
        plan: &OrchestrationPlan,
        now: Timestamp,
        depth: usize,
        executor: &dyn TurnExecutor,
    ) -> Result<RunReport> {
        if depth > MAX_DELEGATION_DEPTH {
            return Err(FamilyClawError::invalid_input(format!(
                "sub-delegation depth {depth} exceeds budget {MAX_DELEGATION_DEPTH}"
            )));
        }

        plan.validate()?;
        let order = plan.topo_order()?;

        let board = self.bridge.board();
        let bus = self.bridge.bus();

        // Solmun → luodun tehtävän kuvaus, jotta riippuvuuksien valmistuminen
        // voidaan tarkistaa. `failed` kerää epäonnistuneet solmut, jotta niiden
        // jälkeläiset jätetään etenemättä (haara pysähtyy).
        let mut node_task: HashMap<NodeId, TaskId> = HashMap::with_capacity(order.len());
        let mut completed: Vec<(NodeId, TaskId)> = Vec::with_capacity(order.len());
        let mut failed: HashSet<NodeId> = HashSet::new();

        for node_id in &order {
            let node = plan.node(node_id).ok_or_else(|| {
                FamilyClawError::not_found(format!("node {node_id} vanished from plan"))
            })?;

            // Jos jokin riippuvuus epäonnistui, tämä solmu peritään
            // epäonnistuneeksi: haara on jo katkennut eikä työtä aloiteta.
            if node.deps.iter().any(|dep| failed.contains(dep)) {
                failed.insert(node_id.clone());
                continue;
            }

            // Solmu on valmis ajettavaksi vain jos KAIKKI riippuvuudet ovat
            // Done. Topologinen järjestys takaa että ne on jo käsitelty, mutta
            // varmistamme tilan taululta (ei oleteta).
            for dep in &node.deps {
                let dep_task = node_task.get(dep).ok_or_else(|| {
                    FamilyClawError::invalid_input(format!(
                        "node {node_id} dependency {dep} was not scheduled"
                    ))
                })?;
                let dep_status = board.get(*dep_task).await.map(|t| t.status);
                if dep_status != Some(TaskStatus::Done) {
                    return Err(FamilyClawError::invalid_input(format!(
                        "node {node_id} dependency {dep} is not Done (was {dep_status:?})"
                    )));
                }
            }

            // Valitse työntekijä, aja vuoro sauman läpi ja vie solmu joko
            // `Done`-tilaan tai merkitse epäonnistuneeksi. Koko per-solmu-logiikka
            // on `drive_node`-apurissa, jotta tämä silmukka pysyy luettavana.
            match self.drive_node(plan, node, node_id, now, executor).await? {
                NodeOutcome::Completed(task_id) => {
                    node_task.insert(node_id.clone(), task_id);
                    completed.push((node_id.clone(), task_id));
                }
                NodeOutcome::Failed => {
                    failed.insert(node_id.clone());
                }
            }
        }

        // Koko työnkulku valmis (vain valmistuneet solmut lasketaan).
        let done_payload = WorkflowDonePayload {
            plan_id: plan.id.clone(),
            node_count: completed.len(),
        };
        let event =
            Event::with_payload(EventKind::Custom(WORKFLOW_DONE.into()), None, &done_payload)
                .unwrap_or_else(|_| Event::new(EventKind::Custom(WORKFLOW_DONE.into()), None));
        bus.publish(event);

        Ok(RunReport {
            plan_id: plan.id.clone(),
            completed,
        })
    }

    /// Vie yhden solmun läpi: valitsee työntekijän, julkaisee [`STEP_ASSIGNED`],
    /// rakentaa vuoron, ajaa sen [`TurnExecutor`]-sauman läpi ja vie tehtävän
    /// joko `Done`-tilaan (hyväksytty toimite) tai merkitsee epäonnistuneeksi
    /// ([`STEP_FAILED`], tehtävä jää ei-`Done`-tilaan).
    ///
    /// # Errors
    /// - [`FamilyClawError::NotFound`] jos kiinnitetty työntekijä ei ole online
    ///   tai sopivaa työntekijää ei löydy.
    /// - Välittää tehtävätaulun siirtymä-/luontivirheet.
    async fn drive_node(
        &self,
        plan: &OrchestrationPlan,
        node: &TaskNode,
        node_id: &NodeId,
        now: Timestamp,
        executor: &dyn TurnExecutor,
    ) -> Result<NodeOutcome> {
        let board = self.bridge.board();
        let bus = self.bridge.bus();

        // Valitse työntekijä: kiinnitetty (jos online) tai sääntöpohjainen.
        let assignee = match node.pinned_assignee {
            Some(pinned) => match self.bridge.registry().liveness_at(pinned, now).await {
                Ok(Liveness::Online) => pinned,
                _ => {
                    return Err(FamilyClawError::not_found(format!(
                        "pinned worker for node {node_id} is not online"
                    )));
                }
            },
            None => self
                .select_worker(node.required_role, &node.required_capabilities, now)
                .await
                .ok_or_else(|| {
                    FamilyClawError::not_found(format!("no eligible worker for node {node_id}"))
                })?,
        };

        // Luo tehtävä, osoita työntekijälle ja aktivoi (Pending → Active).
        let task = board.create(node.title.clone(), Some(assignee)).await?;
        board.update_status(task.id, TaskStatus::Active).await?;

        // Julkaise osoitustapahtuma.
        let assigned_payload = StepPayload {
            plan_id: plan.id.clone(),
            node_id: node_id.0.clone(),
            task_id: task.id.to_string(),
            assignee: assignee.to_string(),
        };
        let event = Event::with_payload(
            EventKind::Custom(STEP_ASSIGNED.into()),
            Some(assignee),
            &assigned_payload,
        )
        .unwrap_or_else(|_| Event::new(EventKind::Custom(STEP_ASSIGNED.into()), Some(assignee)));
        bus.publish(event);

        // Rakenna vuoro ja delegoi sauman kautta. Suorittajan virhe EI jää
        // roikkumaan: se tulkitaan ei-hyväksyttäväksi toimitteeksi.
        let turn = OrchestratedTurn::new(
            plan.id.clone(),
            node_id.clone(),
            task.id,
            assignee,
            node.title.clone(),
            node.description.clone(),
            Self::turn_input(node),
            now,
        );
        let acceptable = match executor.execute(turn).await {
            Ok(deliverable) => {
                Self::deliverable_accepted(node.capability.as_ref(), deliverable, now).await
            }
            Err(_) => false,
        };

        if acceptable {
            // Active → Done (laillinen).
            board.update_status(task.id, TaskStatus::Done).await?;
            return Ok(NodeOutcome::Completed(task.id));
        }

        // Epäonnistui: tehtävä jää ei-Done-tilaan. Julkaise step_failed.
        let failed_payload = StepFailedPayload {
            plan_id: plan.id.clone(),
            node_id: node_id.0.clone(),
            task_id: task.id.to_string(),
            assignee: assignee.to_string(),
        };
        let event = Event::with_payload(
            EventKind::Custom(STEP_FAILED.into()),
            Some(assignee),
            &failed_payload,
        )
        .unwrap_or_else(|_| Event::new(EventKind::Custom(STEP_FAILED.into()), Some(assignee)));
        bus.publish(event);
        Ok(NodeOutcome::Failed)
    }

    /// Rakentaa solmusta suoritussauman koneluettavan syötteen.
    ///
    /// Aloittaa otsikosta ja kuvauksesta. Jos kuvaus jäsentyy JSON-objektiksi,
    /// sen avaimet nostetaan myös syötteen juureen, jolloin rakenteellinen
    /// solmun syöte (esim. `{"brand": "...", "audience": "..."}`) virtaa
    /// suorittajalle sellaisenaan.
    fn turn_input(node: &TaskNode) -> serde_json::Value {
        let mut input = serde_json::Map::new();
        input.insert(
            "title".to_string(),
            serde_json::Value::String(node.title.clone()),
        );
        input.insert(
            "description".to_string(),
            serde_json::Value::String(node.description.clone()),
        );
        if let Ok(serde_json::Value::Object(fields)) =
            serde_json::from_str::<serde_json::Value>(&node.description)
        {
            for (k, v) in fields {
                input.insert(k, v);
            }
        }
        serde_json::Value::Object(input)
    }

    /// Todentaa toimitteen solmun kykyä vasten (jos kyky on annettu).
    ///
    /// Kun `capability` on `None`, mikä tahansa toimite hyväksytään (simuloitu
    /// polku). Kun kyky on annettu, ajetaan kertakäyttöisen
    /// [`ContractBoard`]-sopimuksen kautta: `propose → accept → fulfill`. Vain
    /// täysi läpäisy (tulosskeema + jälkiehdot) palauttaa `true`; mikä tahansa
    /// rikkomus tai sopimusvirhe palauttaa `false`.
    async fn deliverable_accepted(
        capability: Option<&Capability>,
        deliverable: Deliverable,
        now: Timestamp,
    ) -> bool {
        let Some(capability) = capability else {
            return true;
        };
        let board = ContractBoard::new();
        let provider = deliverable.from;
        // Käytä kyvyn syöteskeeman täyttävää tyhjää syötettä silloin kun se on
        // tyhjä; muutoin ehdotus validoidaan annettua kyvyn syötettä vasten.
        let proposed = board
            .propose(capability, provider, provider, serde_json::json!({}), now)
            .await;
        let Ok(contract) = proposed else {
            return false;
        };
        if board.accept(contract.id, now).await.is_err() {
            return false;
        }
        board.fulfill(contract.id, deliverable, now).await.is_ok()
    }
}

/// Hyötykuorma `orchestration.step_assigned`-tapahtumalle.
#[derive(Debug, Serialize, Deserialize)]
struct StepPayload {
    plan_id: String,
    node_id: String,
    task_id: String,
    assignee: String,
}

/// Hyötykuorma `orchestration.step_failed`-tapahtumalle.
#[derive(Debug, Serialize, Deserialize)]
struct StepFailedPayload {
    plan_id: String,
    node_id: String,
    task_id: String,
    assignee: String,
}

/// Hyötykuorma `orchestration.workflow_done`-tapahtumalle.
#[derive(Debug, Serialize, Deserialize)]
struct WorkflowDonePayload {
    plan_id: String,
    node_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentInfo, HostKind};
    use familyclaw_core::time;

    fn ts(secs: i64) -> Timestamp {
        time::from_unix_secs(secs).expect("valid unix seconds")
    }

    async fn online_worker(
        bridge: &FamilyBridge,
        role: AgentRole,
        caps: &[&str],
        now: Timestamp,
    ) -> AgentId {
        let info = AgentInfo::new(AgentId::new(), "w", role, HostKind::Local)
            .with_capabilities(caps.iter().copied());
        let id = info.id;
        bridge.register_agent(info).await.expect("register");
        bridge.heartbeat(id, now).await.expect("heartbeat");
        id
    }

    #[test]
    fn validate_rejects_duplicate_node_id() {
        let plan = OrchestrationPlan::new(
            "p",
            vec![TaskNode::new("a", "t", ""), TaskNode::new("a", "t2", "")],
        );
        let err = plan.validate().expect_err("dup");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn validate_rejects_dangling_dependency() {
        let plan =
            OrchestrationPlan::new("p", vec![TaskNode::new("a", "t", "").with_deps(["ghost"])]);
        let err = plan.validate().expect_err("dangling");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn validate_rejects_self_dependency() {
        let plan = OrchestrationPlan::new("p", vec![TaskNode::new("a", "t", "").with_deps(["a"])]);
        let err = plan.validate().expect_err("self");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn validate_rejects_cycle() {
        let plan = OrchestrationPlan::new(
            "p",
            vec![
                TaskNode::new("a", "ta", "").with_deps(["b"]),
                TaskNode::new("b", "tb", "").with_deps(["a"]),
            ],
        );
        let err = plan.validate().expect_err("cycle");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
        assert!(err.to_string().contains("cycle"));
    }

    #[test]
    fn topo_order_is_deterministic_linear() {
        // c -> b -> a (deps), joten järjestys a, b, c.
        let plan = OrchestrationPlan::new(
            "p",
            vec![
                TaskNode::new("c", "tc", "").with_deps(["b"]),
                TaskNode::new("b", "tb", "").with_deps(["a"]),
                TaskNode::new("a", "ta", ""),
            ],
        );
        let order = plan.topo_order().expect("order");
        assert_eq!(
            order,
            vec![NodeId::new("a"), NodeId::new("b"), NodeId::new("c")]
        );
    }

    #[test]
    fn topo_order_ties_break_by_node_id() {
        // a -> {b, c} -> d. b ja c ovat tasapelissä → aakkosjärjestys.
        let plan = OrchestrationPlan::new(
            "p",
            vec![
                TaskNode::new("a", "ta", ""),
                TaskNode::new("c", "tc", "").with_deps(["a"]),
                TaskNode::new("b", "tb", "").with_deps(["a"]),
                TaskNode::new("d", "td", "").with_deps(["b", "c"]),
            ],
        );
        let order = plan.topo_order().expect("order");
        assert_eq!(
            order,
            vec![
                NodeId::new("a"),
                NodeId::new("b"),
                NodeId::new("c"),
                NodeId::new("d")
            ]
        );
    }

    #[test]
    fn topo_order_stable_across_calls() {
        let plan = OrchestrationPlan::new(
            "p",
            vec![
                TaskNode::new("z", "tz", "").with_deps(["a"]),
                TaskNode::new("m", "tm", "").with_deps(["a"]),
                TaskNode::new("a", "ta", ""),
            ],
        );
        let o1 = plan.topo_order().expect("o1");
        let o2 = plan.topo_order().expect("o2");
        assert_eq!(o1, o2);
        assert_eq!(o1[0], NodeId::new("a"));
    }

    #[tokio::test]
    async fn select_worker_matches_role() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let exec = online_worker(&bridge, AgentRole::Executor, &[], now).await;
        let _scout = online_worker(&bridge, AgentRole::Scout, &[], now).await;

        let chosen = bridge_select(&bridge, Some(AgentRole::Executor), &[], now).await;
        assert_eq!(chosen, Some(exec));
    }

    #[tokio::test]
    async fn select_worker_requires_capability_superset() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        // Agentilla on vain "browser"; vaaditaan myös "system.run" → ei kelpaa.
        let _weak = online_worker(&bridge, AgentRole::Executor, &["browser"], now).await;
        let strong = online_worker(
            &bridge,
            AgentRole::Executor,
            &["browser", "system.run"],
            now,
        )
        .await;

        let chosen = bridge_select(
            &bridge,
            Some(AgentRole::Executor),
            &["system.run".to_string()],
            now,
        )
        .await;
        assert_eq!(chosen, Some(strong));
    }

    #[tokio::test]
    async fn select_worker_excludes_offline() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        // Heartbeat vanha → offline hetkellä now.
        let info = AgentInfo::new(AgentId::new(), "old", AgentRole::Executor, HostKind::Local);
        let id = info.id;
        bridge.register_agent(info).await.expect("register");
        bridge.heartbeat(id, ts(0)).await.expect("hb"); // 1000s vanha > 30s timeout

        let chosen = bridge_select(&bridge, Some(AgentRole::Executor), &[], now).await;
        assert_eq!(chosen, None);
    }

    #[tokio::test]
    async fn select_worker_tie_breaks_by_fewest_in_flight_then_id() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let a = AgentId::from_uuid(uuid::Uuid::from_u128(1));
        let b = AgentId::from_uuid(uuid::Uuid::from_u128(2));
        for id in [a, b] {
            let info = AgentInfo::new(id, "w", AgentRole::Executor, HostKind::Local);
            bridge.register_agent(info).await.expect("reg");
            bridge.heartbeat(id, now).await.expect("hb");
        }
        // Anna a:lle yksi keskeneräinen tehtävä → b:llä on vähemmän.
        let t = bridge.create_task("busy", Some(a)).await.expect("task");
        bridge
            .update_task_status(t.id, TaskStatus::Active)
            .await
            .expect("active");

        let chosen = bridge_select(&bridge, Some(AgentRole::Executor), &[], now).await;
        assert_eq!(chosen, Some(b));

        // Kun a:n tehtävä on valmis (terminaalinen), tasapeli ratkeaa id:llä → a.
        bridge
            .update_task_status(t.id, TaskStatus::Done)
            .await
            .expect("done");
        let chosen2 = bridge_select(&bridge, Some(AgentRole::Executor), &[], now).await;
        assert_eq!(chosen2, Some(a));
    }

    // Apufunktio testeille (Orchestrator::select_worker ottaa &[String]).
    async fn bridge_select(
        bridge: &FamilyBridge,
        role: Option<AgentRole>,
        caps: &[String],
        now: Timestamp,
    ) -> Option<AgentId> {
        Orchestrator::new(bridge.clone())
            .select_worker(role, caps, now)
            .await
    }

    #[tokio::test]
    async fn run_linear_a_b_c() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let _w = online_worker(&bridge, AgentRole::Executor, &[], now).await;
        let mut sub = bridge.subscribe();

        let plan = OrchestrationPlan::new(
            "linear",
            vec![
                TaskNode::new("a", "ta", "").with_role(AgentRole::Executor),
                TaskNode::new("b", "tb", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["a"]),
                TaskNode::new("c", "tc", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["b"]),
            ],
        );
        let orch = Orchestrator::new(bridge.clone());
        let report = orch.run(&plan, now).await.expect("run");
        assert_eq!(report.completed.len(), 3);
        assert_eq!(report.completed[0].0, NodeId::new("a"));
        assert_eq!(report.completed[2].0, NodeId::new("c"));

        // Kaikki tehtävät Done.
        for (_node, task_id) in &report.completed {
            let t = bridge.board().get(*task_id).await.expect("task");
            assert_eq!(t.status, TaskStatus::Done);
        }

        // Tapahtumia tuli: 3x step_assigned + 1x workflow_done (vähintään).
        let mut step = 0;
        let mut done = 0;
        while let Ok(Some(ev)) = sub.try_recv() {
            match &ev.kind {
                EventKind::Custom(name) if name == STEP_ASSIGNED => step += 1,
                EventKind::Custom(name) if name == WORKFLOW_DONE => done += 1,
                _ => {}
            }
        }
        assert_eq!(step, 3);
        assert_eq!(done, 1);
    }

    #[tokio::test]
    async fn run_diamond_a_bc_d() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let _w = online_worker(&bridge, AgentRole::Executor, &[], now).await;

        let plan = OrchestrationPlan::new(
            "diamond",
            vec![
                TaskNode::new("a", "ta", "").with_role(AgentRole::Executor),
                TaskNode::new("b", "tb", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["a"]),
                TaskNode::new("c", "tc", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["a"]),
                TaskNode::new("d", "td", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["b", "c"]),
            ],
        );
        let orch = Orchestrator::new(bridge.clone());
        let report = orch.run(&plan, now).await.expect("run");
        assert_eq!(report.completed.len(), 4);
        // a ensin, d viimeisenä; b ja c välissä aakkosjärjestyksessä.
        let order: Vec<NodeId> = report.completed.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(
            order,
            vec![
                NodeId::new("a"),
                NodeId::new("b"),
                NodeId::new("c"),
                NodeId::new("d")
            ]
        );
    }

    /// Phase 5 (D3): Orkestraattori koordinoi **≥2 live-agenttia** reitittäen
    /// solmut KYVYKKYYDEN mukaan oikealle työntekijälle. Tämä on aito
    /// multi-agent-koordinointi joka toimii nykyrakenteella: kahdella eri-
    /// kykyisellä työntekijällä eri solmut menevät eri agenteille
    /// ([`Orchestrator::select_worker`] suodattaa `required_capabilities`:n
    /// mukaan). `TurnExecutor`-sauma (tässä mock) on sama jonka läpi
    /// `LiveTurnExecutor` ajaa tuotannossa.
    ///
    /// Huom (rehellinen rajaus): solmut ajetaan **sekventiaalisesti** loppuun
    /// (kukin Done ennen seuraavaa), joten pelkkä kuorma-tasapainotus ei vielä
    /// hajauta riippumattomia solmuja rinnakkain — rinnakkaissuoritus on osa
    /// Phase 5:n isompaa työtä (per-node journal ownership). Kyvykkyys­reititys
    /// sen sijaan koordinoi ≥2 agenttia jo nyt.
    #[tokio::test]
    async fn run_routes_nodes_to_capable_workers() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        // Työntekijä A osaa "sql", työntekijä B osaa "vision".
        let a = AgentId::from_uuid(uuid::Uuid::from_u128(1));
        let b = AgentId::from_uuid(uuid::Uuid::from_u128(2));
        let info_a = AgentInfo::new(a, "w-sql", AgentRole::Executor, HostKind::Local)
            .with_capabilities(["sql"]);
        let info_b = AgentInfo::new(b, "w-vision", AgentRole::Executor, HostKind::Local)
            .with_capabilities(["vision"]);
        bridge.register_agent(info_a).await.expect("reg a");
        bridge.register_agent(info_b).await.expect("reg b");
        bridge.heartbeat(a, now).await.expect("hb a");
        bridge.heartbeat(b, now).await.expect("hb b");

        // Kaksi solmua: toinen vaatii "sql" → A, toinen "vision" → B.
        let plan = OrchestrationPlan::new(
            "capability-routed",
            vec![
                TaskNode::new("q", "query", "")
                    .with_role(AgentRole::Executor)
                    .with_capabilities(["sql"]),
                TaskNode::new("img", "analyze", "")
                    .with_role(AgentRole::Executor)
                    .with_capabilities(["vision"]),
            ],
        );
        let orch = Orchestrator::new(bridge.clone());
        let report = orch.run(&plan, now).await.expect("run");
        assert_eq!(report.completed.len(), 2);

        // Kumpikin työntekijä sai TÄSMÄLLEEN kykyään vastaavan solmun → ≥2
        // agenttia koordinoitiin kyvykkyysreitityksellä.
        let a_tasks = bridge.board().list_for_assignee(a).await.len();
        let b_tasks = bridge.board().list_for_assignee(b).await.len();
        assert_eq!(a_tasks, 1, "sql-työntekijä sai sql-solmun, sai {a_tasks}");
        assert_eq!(
            b_tasks, 1,
            "vision-työntekijä sai vision-solmun, sai {b_tasks}"
        );
    }

    #[tokio::test]
    async fn run_errors_when_no_eligible_worker() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        // Ei yhtään agenttia.
        let plan = OrchestrationPlan::new(
            "p",
            vec![TaskNode::new("a", "ta", "").with_role(AgentRole::Executor)],
        );
        let orch = Orchestrator::new(bridge);
        let err = orch.run(&plan, now).await.expect_err("no worker");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn run_uses_pinned_assignee() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let pinned = online_worker(&bridge, AgentRole::Scout, &[], now).await;
        let plan = OrchestrationPlan::new(
            "p",
            vec![TaskNode::new("a", "ta", "").with_pinned_assignee(pinned)],
        );
        let orch = Orchestrator::new(bridge.clone());
        let report = orch.run(&plan, now).await.expect("run");
        let (_n, task_id) = &report.completed[0];
        let t = bridge.board().get(*task_id).await.expect("task");
        assert_eq!(t.assignee, Some(pinned));
    }

    #[tokio::test]
    async fn run_pinned_offline_errors() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let info = AgentInfo::new(AgentId::new(), "p", AgentRole::Scout, HostKind::Local);
        let pinned = info.id;
        bridge.register_agent(info).await.expect("reg");
        // Ei heartbeatia → Unknown, ei Online.
        let plan = OrchestrationPlan::new(
            "p",
            vec![TaskNode::new("a", "ta", "").with_pinned_assignee(pinned)],
        );
        let orch = Orchestrator::new(bridge);
        let err = orch.run(&plan, now).await.expect_err("offline pinned");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn run_nested_exceeds_depth_budget() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let plan = OrchestrationPlan::new("p", vec![TaskNode::new("a", "ta", "")]);
        let orch = Orchestrator::new(bridge);
        let err = orch
            .run_nested(&plan, now, MAX_DELEGATION_DEPTH + 1)
            .await
            .expect_err("over budget");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn run_is_deterministic_same_plan_and_now() {
        let now = ts(1000);
        let make = || async move {
            let bridge = FamilyBridge::new();
            // Kaksi yhtä kelvollista työntekijää, kiinteät id:t.
            for n in 1..=2u128 {
                let id = AgentId::from_uuid(uuid::Uuid::from_u128(n));
                let info = AgentInfo::new(id, "w", AgentRole::Executor, HostKind::Local);
                bridge.register_agent(info).await.expect("reg");
                bridge.heartbeat(id, now).await.expect("hb");
            }
            let plan = OrchestrationPlan::new(
                "p",
                vec![
                    TaskNode::new("a", "ta", "").with_role(AgentRole::Executor),
                    TaskNode::new("b", "tb", "")
                        .with_role(AgentRole::Executor)
                        .with_deps(["a"]),
                ],
            );
            let orch = Orchestrator::new(bridge.clone());
            let report = orch.run(&plan, now).await.expect("run");
            // Palauta solmu→assignee-kartta vertailua varten.
            let mut out = Vec::new();
            for (node, task_id) in report.completed {
                let t = bridge.board().get(task_id).await.expect("task");
                out.push((node, t.assignee));
            }
            out
        };
        let r1 = make().await;
        let r2 = make().await;
        assert_eq!(r1, r2);
        // Ensimmäinen työntekijä (vähiten in-flight, pienin id) on u128=1.
        assert_eq!(r1[0].1, Some(AgentId::from_uuid(uuid::Uuid::from_u128(1))));
    }

    // =======================================================================
    // run_with — orkesteri reititettynä TurnExecutor-sauman läpi
    // =======================================================================

    use crate::contract::{Capability, Field, FieldType, Schema};
    use crate::executor::{MockFailure, MockTurnExecutor};

    /// HomepageDesign-muotoinen tulosskeema johon mockin onnistuva toimite
    /// sopii mutta `failing()`-toimite ei (puuttuva `headline`).
    fn homepage_capability() -> Capability {
        Capability::new(
            "design_homepage",
            Schema::empty(),
            Schema::new(vec![
                Field::required("headline", FieldType::Str),
                Field::required("sections", FieldType::Arr),
                Field::required("cta", FieldType::Str),
            ]),
        )
    }

    #[tokio::test]
    async fn run_with_mock_executor_runs_linear_plan_to_completion() {
        // run_with + MockTurnExecutor ajaa A→B-suunnitelman loppuun: molemmat
        // tehtävät Done, raportin järjestys [A, B].
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let _w = online_worker(&bridge, AgentRole::Executor, &[], now).await;

        let plan = OrchestrationPlan::new(
            "linear",
            vec![
                TaskNode::new("a", "ta", "").with_role(AgentRole::Executor),
                TaskNode::new("b", "tb", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["a"]),
            ],
        );

        let orch = Orchestrator::new(bridge.clone());
        let executor = MockTurnExecutor::new();
        let report = orch
            .run_with(&plan, now, &executor)
            .await
            .expect("run_with");

        assert_eq!(report.completed.len(), 2);
        assert_eq!(report.completed[0].0, NodeId::new("a"));
        assert_eq!(report.completed[1].0, NodeId::new("b"));
        for (_node, task_id) in &report.completed {
            let t = bridge.board().get(*task_id).await.expect("task");
            assert_eq!(t.status, TaskStatus::Done);
        }
    }

    #[tokio::test]
    async fn run_with_failing_executor_leaves_node_non_done_and_blocks_dependents() {
        // run_with + MockTurnExecutor::failing() (skeemarikkomus) solmulla joka
        // KANTAA kykyä → solmu jää ei-Done, eikä sen jälkeläistä etenetä.
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let worker = online_worker(&bridge, AgentRole::Executor, &[], now).await;
        let mut sub = bridge.subscribe();

        let plan = OrchestrationPlan::new(
            "fails",
            vec![
                TaskNode::new("a", "ta", "")
                    .with_role(AgentRole::Executor)
                    .with_capability(homepage_capability()),
                TaskNode::new("b", "tb", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["a"]),
            ],
        );

        let orch = Orchestrator::new(bridge.clone());
        // failing() tuottaa toimitteen ilman headline-kenttää → fulfill kaatuu.
        let executor = MockTurnExecutor::failing();
        let report = orch
            .run_with(&plan, now, &executor)
            .await
            .expect("run_with");

        // Mikään solmu ei valmistunut.
        assert!(report.completed.is_empty(), "no node should complete");

        // A:lle luotiin tehtävä mutta se EI ole Done (jäi Active-tilaan).
        let a_tasks = bridge
            .board()
            .list_for_assignee(worker)
            .await
            .into_iter()
            .filter(|t| t.title == "ta")
            .collect::<Vec<_>>();
        assert_eq!(a_tasks.len(), 1, "A task was created");
        assert_ne!(a_tasks[0].status, TaskStatus::Done, "A must not be Done");
        assert_eq!(a_tasks[0].status, TaskStatus::Active);

        // B:tä (A:n jälkeläinen) ei koskaan osoitettu → ei tehtävää otsikolla tb.
        let b_tasks = bridge
            .board()
            .list_for_assignee(worker)
            .await
            .into_iter()
            .filter(|t| t.title == "tb")
            .collect::<Vec<_>>();
        assert!(b_tasks.is_empty(), "dependent B must not be scheduled");

        // step_failed julkaistiin A:lle; step_assigned vain A:lle (ei B:lle).
        let mut assigned = 0;
        let mut step_failed = 0;
        while let Ok(Some(ev)) = sub.try_recv() {
            match &ev.kind {
                EventKind::Custom(name) if name == STEP_ASSIGNED => assigned += 1,
                EventKind::Custom(name) if name == STEP_FAILED => step_failed += 1,
                _ => {}
            }
        }
        assert_eq!(assigned, 1, "only A is assigned");
        assert_eq!(step_failed, 1, "A emits step_failed");
    }

    #[tokio::test]
    async fn run_delegates_to_run_with_mock_identically() {
        // run() ja run_with(MockTurnExecutor::default()) tuottavat saman
        // tuloksen — delegointi pitää (taaksepäinyhteensopivuus).
        let now = ts(1000);
        let build = |use_default_run: bool| async move {
            let bridge = FamilyBridge::new();
            for n in 1..=2u128 {
                let id = AgentId::from_uuid(uuid::Uuid::from_u128(n));
                let info = AgentInfo::new(id, "w", AgentRole::Executor, HostKind::Local);
                bridge.register_agent(info).await.expect("reg");
                bridge.heartbeat(id, now).await.expect("hb");
            }
            let plan = OrchestrationPlan::new(
                "p",
                vec![
                    TaskNode::new("a", "ta", "").with_role(AgentRole::Executor),
                    TaskNode::new("b", "tb", "")
                        .with_role(AgentRole::Executor)
                        .with_deps(["a"]),
                ],
            );
            let orch = Orchestrator::new(bridge.clone());
            let report = if use_default_run {
                orch.run(&plan, now).await.expect("run")
            } else {
                let executor = MockTurnExecutor::default();
                orch.run_with(&plan, now, &executor)
                    .await
                    .expect("run_with")
            };
            let mut out = Vec::new();
            for (node, task_id) in report.completed {
                let t = bridge.board().get(task_id).await.expect("task");
                out.push((node, t.status, t.assignee));
            }
            out
        };
        let via_run = build(true).await;
        let via_run_with = build(false).await;
        assert_eq!(via_run, via_run_with);
        assert_eq!(via_run.len(), 2);
        // Molemmat solmut Done molemmissa poluissa.
        assert!(via_run.iter().all(|(_, s, _)| *s == TaskStatus::Done));
    }

    #[tokio::test]
    async fn run_with_erroring_executor_marks_node_failed_without_hanging() {
        // Err-palauttava suorittaja → solmu epäonnistuu (ei Done), ajo palaa
        // ilman roikkumista. Yksisolmuinen suunnitelma riittää.
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let w = online_worker(&bridge, AgentRole::Executor, &[], now).await;

        let plan = OrchestrationPlan::new(
            "errs",
            vec![TaskNode::new("a", "ta", "").with_role(AgentRole::Executor)],
        );

        let orch = Orchestrator::new(bridge.clone());
        let executor = MockTurnExecutor::with_failure(MockFailure::Error);
        // Ei roikkumista: kutsu palaa Ok-raportilla jossa ei valmistuneita.
        let report = orch
            .run_with(&plan, now, &executor)
            .await
            .expect("run_with");
        assert!(report.completed.is_empty(), "node must not complete on Err");

        // Tehtävä luotiin ja jäi ei-Done-tilaan.
        let tasks = bridge.board().list_for_assignee(w).await;
        assert_eq!(tasks.len(), 1);
        assert_ne!(tasks[0].status, TaskStatus::Done);
    }

    #[tokio::test]
    async fn run_with_capability_node_reaches_done_when_deliverable_valid() {
        // Solmu jolla on kyky + onnistuva mock → toimite läpäisee fulfillin →
        // Done. Kuvauksessa brand/audience ohjaa mockin tuottamaan
        // HomepageDesign-muotoisen (skeeman täyttävän) toimitteen.
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let _w = online_worker(&bridge, AgentRole::Executor, &[], now).await;

        let plan = OrchestrationPlan::new(
            "ok",
            vec![
                TaskNode::new("a", "ta", r#"{"brand":"DuckUps","audience":"founders"}"#)
                    .with_role(AgentRole::Executor)
                    .with_capability(homepage_capability()),
            ],
        );

        let orch = Orchestrator::new(bridge.clone());
        let executor = MockTurnExecutor::new();
        let report = orch
            .run_with(&plan, now, &executor)
            .await
            .expect("run_with");
        assert_eq!(report.completed.len(), 1);
        let (_n, task_id) = &report.completed[0];
        let t = bridge.board().get(*task_id).await.expect("task");
        assert_eq!(t.status, TaskStatus::Done);
    }
}
