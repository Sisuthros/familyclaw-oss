//! [`EventRecorder`] — tilaa siltakerroksen tapahtumaväylän ja muuntaa
//! tapahtumat mittaripäivityksiksi.
//!
//! Tallennin on **vain lukeva**: se tilaa [`EventBus`]in
//! ([`FamilyBridge::subscribe`]) ja kuluttaa tapahtumia, mutta ei koskaan
//! julkaise väylälle. Tapahtumalaji ([`EventKind`]) kuvataan
//! [`MetricsRegistry`]-päivitykseksi. Tuntemattomat ja tulevat lajit (mukaan
//! lukien [`EventKind::Custom`] joita ei erikseen tunnisteta) **ohitetaan**
//! `_ => {}`-haarassa — näin uudet tapahtumatyypit eivät koskaan riko
//! tallenninta (eteenpäin-yhteensopivuus).
//!
//! ## Tapahtuma → mittari -kartta (mitkä sarjat ovat eläviä)
//! Tallennin kasvattaa **vain** alla luetellut sarjat. Muut laivueen
//! oletussarjat pysyvät nollassa kunnes niille tuotetaan vastaava tapahtuma.
//!
//! | [`EventKind`] | Mittari |
//! |---|---|
//! | `TaskCreated` | `tasks_created` (laskuri +1) |
//! | `TaskHandedOff` | `task_handoffs` (laskuri +1) |
//! | `AgentRegistered` | `agents_online` (gauge +1) |
//! | `AgentDeregistered` | `agents_online` (gauge -1) |
//! | `Custom("task.completed" \| "orchestration.task_completed")` | `tasks_completed` (+1) |
//! | `Custom("contract.proposed")` | `contract_proposed` (+1) |
//! | `Custom("contract.fulfilled")` | `contract_fulfilled` (+1) |
//! | `Custom("contract.breached")` | `contract_breached` (+1) |
//! | `Custom("agent.turn" \| "orchestration.agent_turn")` | `agent_turns` (+1) |
//! | `Custom("llm.call")` | `llm_calls` (+1) |
//! | `Custom("llm.fallback")` | `llm_fallbacks` (+1) |
//! | `Custom("durable.replay")` | `durable_replays` (+1) |
//! | `Custom("workflow.step_completed" \| "orchestration.workflow_step_completed")` | `workflow_steps_completed` (+1) |
//!
//! `TaskStatusChanged` ja `AgentHeartbeat` eivät kartoitu omaan mittariin.
//!
//! ## Custom-tapahtumat
//! Orkestrointi- ja sopimuskerros lähettää koordinaation
//! [`EventKind::Custom`]-tapahtumina vakaalla etuliitteellä
//! (`contract.*`, `orchestration.*`, `workflow.*`). Tallennin tunnistaa
//! tunnetut etiketit ja kasvattaa vastaavia laskureita; tuntemattomat
//! etiketit ohitetaan turvallisesti.
//!
//! ## Käyttö
//! ```
//! use familyclaw_bridge::FamilyBridge;
//! use familyclaw_observability::{EventRecorder, MetricsRegistry};
//!
//! # async fn run() -> familyclaw_core::Result<()> {
//! let bridge = FamilyBridge::new();
//! let metrics = MetricsRegistry::with_fleet_defaults();
//! let mut recorder = EventRecorder::new(&bridge, metrics.clone());
//!
//! // Tuota tapahtuma...
//! bridge.create_task("seed", None).await?;
//! // ...ja valuta se mittareihin.
//! recorder.drain_once().await;
//! # Ok(())
//! # }
//! ```
//!
//! [`EventBus`]: familyclaw_bridge::EventBus

use familyclaw_bridge::{EventKind, EventSubscriber, FamilyBridge};

use crate::metrics::{
    MetricsRegistry, COUNTER_AGENT_TURNS, COUNTER_CONTRACT_BREACHED, COUNTER_CONTRACT_FULFILLED,
    COUNTER_CONTRACT_PROPOSED, COUNTER_DURABLE_REPLAYS, COUNTER_LLM_CALLS, COUNTER_LLM_FALLBACKS,
    COUNTER_TASKS_COMPLETED, COUNTER_TASKS_CREATED, COUNTER_TASK_HANDOFFS,
    COUNTER_WORKFLOW_STEPS_COMPLETED, GAUGE_AGENTS_ONLINE,
};

/// Tilaa tapahtumaväylän ja päivittää mittareita tapahtumien perusteella.
///
/// Pidä yksi tallennin elossa koko gatewayn eliniän. Tilaaja näkee vain
/// *tilauksen jälkeen* julkaistut tapahtumat (ks. [`EventBus`]-semantiikka),
/// joten luo tallennin ennen kuin liikennettä alkaa syntyä.
///
/// [`EventBus`]: familyclaw_bridge::EventBus
#[derive(Debug)]
pub struct EventRecorder {
    subscriber: EventSubscriber,
    metrics: MetricsRegistry,
}

impl EventRecorder {
    /// Luo tallentimen joka tilaa annetun sillan väylän ja kirjaa annettuun
    /// rekisteriin.
    #[must_use]
    pub fn new(bridge: &FamilyBridge, metrics: MetricsRegistry) -> Self {
        Self {
            subscriber: bridge.subscribe(),
            metrics,
        }
    }

    /// Pääsy tallentimen mittarirekisteriin.
    #[must_use]
    pub fn metrics(&self) -> &MetricsRegistry {
        &self.metrics
    }

    /// Valuttaa kaikki *tällä hetkellä jonossa* olevat tapahtumat estämättä
    /// ja palauttaa montako tapahtumaa käsiteltiin.
    ///
    /// Tämä ei odota uusia tapahtumia — se vain tyhjentää sen mitä on heti
    /// saatavilla. Jos tilaaja jäi jälkeen ja tapahtumia pudotettiin, pudotus
    /// ohitetaan (mittarit eivät voi paniikata tästä) ja valutus jatkuu.
    ///
    /// `async` on osa vakaata rajapintaa ([`run`]:n rinnalla) eikä siksi
    /// poistettavissa, vaikka nykytoteutus ei `await`-tä — tämä sallii
    /// myöhemmin lisättävän odottavan vastapainevariantin ilman API-muutosta.
    ///
    /// [`run`]: EventRecorder::run
    #[allow(clippy::unused_async)]
    pub async fn drain_once(&mut self) -> usize {
        let mut processed = 0usize;
        // `try_recv` palauttaa `Ok(Some)` niin kauan kuin jonossa on tapahtumia.
        // `Ok(None)` (tyhjä) ja `Err(_)` (lagged/closed) lopettavat silmukan
        // siististi — pudotettuja tapahtumia ei lasketa eikä paniikkia synny.
        while let Ok(Some(event)) = self.subscriber.try_recv() {
            self.record(&event.kind);
            processed += 1;
        }
        processed
    }

    /// Estävä silmukka: odottaa ja käsittelee tapahtumia kunnes väylä sulkeutuu.
    ///
    /// Soveltuu omistettuun taustatehtävään (`tokio::spawn`). Palaa kun väylä
    /// on suljettu (kaikki lähettäjät pudotettu). Lagged-tilanteet ohitetaan
    /// ja kuuntelu jatkuu.
    pub async fn run(mut self) {
        loop {
            match self.subscriber.recv().await {
                Ok(event) => self.record(&event.kind),
                // Suljettu → lopeta; lagged → jatka.
                Err(err) => {
                    let msg = err.to_string();
                    if msg.contains("closed") {
                        break;
                    }
                    // lagged: jatka kuuntelua.
                }
            }
        }
    }

    /// Muuntaa yhden tapahtumalajin mittaripäivitykseksi.
    ///
    /// Tunnettujen ydinlajien lisäksi tunnistetaan vakaat
    /// [`EventKind::Custom`]-etiketit. Tuntemattomat lajit ohitetaan
    /// (`_ => {}`) — tämä on tahallinen eteenpäin-yhteensopivuusvara.
    fn record(&self, kind: &EventKind) {
        match kind {
            EventKind::TaskCreated => self.metrics.counter(COUNTER_TASKS_CREATED).inc(),
            EventKind::TaskHandedOff => self.metrics.counter(COUNTER_TASK_HANDOFFS).inc(),
            // Agentin rekisteröinti/poisto liikuttaa `agents_online`-gaugea:
            // rekisteröinti +1, poisto -1. Tämä on hetkellinen arvo (gauge), ei
            // kumulatiivinen laskuri — se voi nousta ja laskea agenttien tullessa
            // ja poistuessa. Runtime julkaisee rekisteröinnin agentin spawnatessa
            // (havainnoitavuussilta), joten gauge heijastaa elävää agenttimäärää.
            EventKind::AgentRegistered => self.metrics.gauge(GAUGE_AGENTS_ONLINE).add(1),
            EventKind::AgentDeregistered => self.metrics.gauge(GAUGE_AGENTS_ONLINE).sub(1),
            EventKind::Custom(label) => self.record_custom(label),
            // Eteenpäin-yhteensopivuus: turvallisesti ohitettavat lajit.
            //
            // - `TaskStatusChanged`: emme tarkastele hyötykuormaa tässä, joten
            //   emme tiedä kohdetilaa; tehtävän valmistuminen kirjataan
            //   erilliseltä Custom-etiketiltä (workflow/orkestrointi).
            // - `AgentHeartbeat`: ei oma mittarinsa (liveness lasketaan
            //   rekisteröinnin perusteella).
            // - Mahdolliset tulevat variantit (`_`).
            _ => {}
        }
    }

    /// Kuvaa vakaan Custom-etiketin mittaripäivitykseksi.
    fn record_custom(&self, label: &str) {
        match label {
            "task.completed" | "orchestration.task_completed" => {
                self.metrics.counter(COUNTER_TASKS_COMPLETED).inc();
            }
            "contract.proposed" => self.metrics.counter(COUNTER_CONTRACT_PROPOSED).inc(),
            "contract.fulfilled" => self.metrics.counter(COUNTER_CONTRACT_FULFILLED).inc(),
            "contract.breached" => self.metrics.counter(COUNTER_CONTRACT_BREACHED).inc(),
            "agent.turn" | "orchestration.agent_turn" => {
                self.metrics.counter(COUNTER_AGENT_TURNS).inc();
            }
            "llm.call" => self.metrics.counter(COUNTER_LLM_CALLS).inc(),
            "llm.fallback" => self.metrics.counter(COUNTER_LLM_FALLBACKS).inc(),
            "durable.replay" => self.metrics.counter(COUNTER_DURABLE_REPLAYS).inc(),
            "workflow.step_completed" | "orchestration.workflow_step_completed" => {
                self.metrics.counter(COUNTER_WORKFLOW_STEPS_COMPLETED).inc();
            }
            // Tuntematon Custom-etiketti → ohita (eteenpäin-yhteensopivuus).
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_bridge::{Event, EventBus, EventKind, FamilyBridge};

    #[tokio::test]
    async fn maps_task_created_event() {
        let bridge = FamilyBridge::new();
        let metrics = MetricsRegistry::with_fleet_defaults();
        let mut recorder = EventRecorder::new(&bridge, metrics.clone());

        bridge.create_task("t", None).await.expect("create");
        let n = recorder.drain_once().await;
        assert_eq!(n, 1);
        assert_eq!(metrics.counter(COUNTER_TASKS_CREATED).get(), 1);
    }

    #[tokio::test]
    async fn maps_handoff_event() {
        use familyclaw_core::ids::AgentId;
        let bridge = FamilyBridge::new();
        let metrics = MetricsRegistry::with_fleet_defaults();
        let mut recorder = EventRecorder::new(&bridge, metrics.clone());

        let from = AgentId::new();
        let to = AgentId::new();
        let task = bridge.create_task("t", Some(from)).await.expect("create");
        bridge
            .handoff_task(task.id, from, to)
            .await
            .expect("handoff");

        recorder.drain_once().await;
        assert_eq!(metrics.counter(COUNTER_TASKS_CREATED).get(), 1);
        assert_eq!(metrics.counter(COUNTER_TASK_HANDOFFS).get(), 1);
    }

    #[tokio::test]
    async fn unknown_custom_variant_is_ignored() {
        // Käytä raakaa väylää julkaistaksesi mielivaltaisen Custom-tapahtuman.
        let bus = EventBus::new();
        let bridge = FamilyBridge::from_parts(
            familyclaw_bridge::AgentRegistry::new(),
            familyclaw_bridge::TaskBoard::new(),
            bus.clone(),
        );
        let metrics = MetricsRegistry::with_fleet_defaults();
        let mut recorder = EventRecorder::new(&bridge, metrics.clone());

        // Täysin tuntematon Custom-etiketti.
        bus.publish(Event::new(
            EventKind::Custom("some.future.event".into()),
            None,
        ));
        let n = recorder.drain_once().await;
        // Tapahtuma KÄSITELTIIN (ei kaatunut), mutta mikään laskuri ei muuttunut.
        assert_eq!(n, 1);
        let out = metrics.prometheus_export();
        // Kaikki laskurit yhä nollassa.
        assert!(out.contains("tasks_created 0"));
        assert!(out.contains("contract_proposed 0"));
    }

    #[tokio::test]
    async fn known_custom_labels_increment_counters() {
        let bus = EventBus::new();
        let bridge = FamilyBridge::from_parts(
            familyclaw_bridge::AgentRegistry::new(),
            familyclaw_bridge::TaskBoard::new(),
            bus.clone(),
        );
        let metrics = MetricsRegistry::with_fleet_defaults();
        let mut recorder = EventRecorder::new(&bridge, metrics.clone());

        for label in [
            "contract.proposed",
            "contract.fulfilled",
            "contract.breached",
            "llm.call",
            "llm.fallback",
            "durable.replay",
            "agent.turn",
            "workflow.step_completed",
            "task.completed",
        ] {
            bus.publish(Event::new(EventKind::Custom(label.into()), None));
        }
        let n = recorder.drain_once().await;
        assert_eq!(n, 9);
        assert_eq!(metrics.counter(COUNTER_CONTRACT_PROPOSED).get(), 1);
        assert_eq!(metrics.counter(COUNTER_CONTRACT_FULFILLED).get(), 1);
        assert_eq!(metrics.counter(COUNTER_CONTRACT_BREACHED).get(), 1);
        assert_eq!(metrics.counter(COUNTER_LLM_CALLS).get(), 1);
        assert_eq!(metrics.counter(COUNTER_LLM_FALLBACKS).get(), 1);
        assert_eq!(metrics.counter(COUNTER_DURABLE_REPLAYS).get(), 1);
        assert_eq!(metrics.counter(COUNTER_AGENT_TURNS).get(), 1);
        assert_eq!(metrics.counter(COUNTER_WORKFLOW_STEPS_COMPLETED).get(), 1);
        assert_eq!(metrics.counter(COUNTER_TASKS_COMPLETED).get(), 1);
    }

    #[tokio::test]
    async fn agent_registration_moves_online_gauge_and_heartbeat_is_ignored() {
        use familyclaw_bridge::{AgentInfo, AgentRole, HostKind};
        use familyclaw_core::ids::AgentId;

        let bridge = FamilyBridge::new();
        let metrics = MetricsRegistry::with_fleet_defaults();
        let mut recorder = EventRecorder::new(&bridge, metrics.clone());

        let info = AgentInfo::new(AgentId::new(), "a", AgentRole::Executor, HostKind::Local);
        let id = info.id;
        bridge.register_agent(info).await.expect("register");
        bridge.heartbeat_now(id).await.expect("heartbeat");

        let n = recorder.drain_once().await;
        // Molemmat tapahtumat käsiteltiin (register + heartbeat).
        assert_eq!(n, 2);
        // Rekisteröinti nosti agents_online-gaugea +1; heartbeatilla ei ole omaa
        // mittaria. Tehtävälaskuri ei muuttunut (eri tapahtumaperhe).
        assert_eq!(metrics.gauge(GAUGE_AGENTS_ONLINE).get(), 1);
        assert_eq!(metrics.counter(COUNTER_TASKS_CREATED).get(), 0);

        // Poisto laskee gaugen takaisin nollaan.
        assert!(
            bridge.deregister_agent(id).await.is_some(),
            "agentti poistui"
        );
        let n2 = recorder.drain_once().await;
        assert_eq!(n2, 1);
        assert_eq!(metrics.gauge(GAUGE_AGENTS_ONLINE).get(), 0);
    }

    #[tokio::test]
    async fn drain_once_on_empty_returns_zero() {
        let bridge = FamilyBridge::new();
        let metrics = MetricsRegistry::new();
        let mut recorder = EventRecorder::new(&bridge, metrics);
        assert_eq!(recorder.drain_once().await, 0);
    }
}
