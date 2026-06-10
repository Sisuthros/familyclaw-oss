//! # familyclaw-bridge
//!
//! Siltakerros (KERROS A, OSS): **agenttirekisteri, tehtävätaulu ja
//! tapahtumaväylä** puhtaana, kuljetuskerroksesta riippumattomana
//! Rust-rajapintana. Design §3: *"käytä olemassa olevaa"* —
//! tämä crate mallintaa olemassa olevan `family-bridge`-MCP:n semantiikan
//! natiivina Rustina, jonka MCP-/HTTP-adapterit voivat myöhemmin kääriä.
//!
//! ## Osat
//! - [`agent`] — [`AgentRegistry`], [`AgentInfo`], liveness/heartbeat.
//! - [`task`] — [`Task`], [`TaskStatus`]-tilakone, [`TaskBoard`] (sis. handoff).
//! - [`event`] — [`Event`], [`EventKind`], publish/subscribe ([`EventBus`]).
//! - [`bridge`] — [`FamilyBridge`] koostaa edellä mainitut ja julkaisee
//!   tapahtumat tilamuutoksista.
//! - [`orchestrator`] — DAG-pohjainen moniagenttiorkesterointi
//!   ([`OrchestrationPlan`], [`Orchestrator`]) joka ohjaa tehtävätaulua
//!   vain laillisin tilasiirtymin.
//! - [`contract`] — tyypitetty FIPA-ContractNet ([`Capability`], [`Contract`],
//!   [`ContractBoard`]) todennettavalla täyttämisellä (skeema + jälkiehdot).
//! - [`contract_bus`] — kuljetuksesta riippumattomat sopimusviestit
//!   ([`ContractMessage`]) puhtaalla serdellä.
//! - [`executor`] — orkesteroinnin ja konkreettisen agentin välinen suoritussauma
//!   ([`TurnExecutor`], [`OrchestratedTurn`]) hermeettisellä [`MockTurnExecutor`]:lla.
//!
//! ## Suunnitteluperiaatteet
//! - **Tokio-pohjainen, säieturvallinen.** Jaettu tila on `Arc<RwLock<…>>`
//!   (rekisteri, taulu) tai `tokio::sync::broadcast` (väylä). Kaikki
//!   julkisivut ovat `Clone` ja jakavat tilansa.
//! - **Ei `unwrap()`/`expect()`/`panic!()` tuotantopolulla.** Kaikki virheet
//!   kulkevat [`familyclaw_core::Result`]- ja [`familyclaw_core::FamilyClawError`]-tyyppien
//!   kautta.
//! - **Tiukka tehtävän tilakone.** Laittomat siirtymät hylätään virheellä,
//!   jotta durable-replay ja konsolidointi pysyvät johdonmukaisina.
//! - **OSS-raja (KERROS A):** ei kovakoodattuja sieluja, avaimia, tokeneita,
//!   IP-osoitteita eikä henkilökohtaisia polkuja. Tyypit ovat geneerisiä.
//!
//! ## Esimerkki
//! ```
//! use familyclaw_bridge::{
//!     AgentInfo, AgentRole, FamilyBridge, HostKind, TaskStatus,
//! };
//! use familyclaw_core::ids::AgentId;
//!
//! # async fn run() -> familyclaw_core::Result<()> {
//! let bridge = FamilyBridge::new();
//! let mut events = bridge.subscribe();
//!
//! // Rekisteröi kaksi agenttia.
//! let a = AgentInfo::new(AgentId::new(), "agent_a", AgentRole::Strategy, HostKind::Local);
//! let b = AgentInfo::new(AgentId::new(), "agent_b", AgentRole::Executor, HostKind::Wsl);
//! let (a_id, b_id) = (a.id, b.id);
//! bridge.register_agent(a).await?;
//! bridge.register_agent(b).await?;
//!
//! // Luo tehtävä, ota työn alle, luovuta toiselle.
//! let task = bridge.create_task("ship the seed", Some(a_id)).await?;
//! bridge.update_task_status(task.id, TaskStatus::Active).await?;
//! let handed = bridge.handoff_task(task.id, a_id, b_id).await?;
//! assert_eq!(handed.assignee, Some(b_id));
//! assert_eq!(handed.status, TaskStatus::Handed);
//! # Ok(())
//! # }
//! ```

pub mod agent;
pub mod bridge;
pub mod contract;
pub mod contract_bus;
pub mod event;
pub mod executor;
pub mod orchestrator;
pub mod task;

pub use agent::{AgentInfo, AgentRegistry, AgentRole, HostKind, Liveness};
pub use bridge::FamilyBridge;
pub use contract::{
    Capability, CapabilityRegistry, Clause, ClauseOp, Contract, ContractBoard, ContractError,
    ContractResult, ContractStatus, Deliverable, Field, FieldType, Schema, SchemaViolation,
};
pub use contract_bus::{ContractMessage, CONTRACT_CUSTOM_NAME};
pub use event::{Event, EventBus, EventKind, EventSubscriber};
pub use executor::{MockFailure, MockTurnExecutor, OrchestratedTurn, TurnExecutor};
pub use orchestrator::{
    NodeId, OrchestrationPlan, Orchestrator, RunReport, TaskNode, MAX_DELEGATION_DEPTH,
    STEP_ASSIGNED, STEP_FAILED, WORKFLOW_DONE,
};
pub use task::{Task, TaskBoard, TaskId, TaskStatus};

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::ids::AgentId;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[tokio::test]
    async fn public_api_is_reexported() {
        // Varmistaa että julkinen pinta on saatavilla juuresta. Jos jokin
        // re-export poistetaan, tämä testi ei käänny.
        let bridge: FamilyBridge = FamilyBridge::new();
        let _registry: &AgentRegistry = bridge.registry();
        let _board: &TaskBoard = bridge.board();
        let _bus: &EventBus = bridge.bus();
        let mut _sub: EventSubscriber = bridge.subscribe();

        let info: AgentInfo = AgentInfo::new(
            AgentId::new(),
            "agent_a",
            AgentRole::Executor,
            HostKind::Local,
        );
        let id = info.id;
        bridge.register_agent(info).await.expect("register");
        assert_eq!(
            bridge.liveness(id).await.expect("liveness"),
            Liveness::Unknown
        );

        let task: Task = bridge.create_task("t", Some(id)).await.expect("create");
        let tid: TaskId = task.id;
        assert!(!tid.is_nil());
        assert_eq!(task.status, TaskStatus::Pending);

        let _ev: Event = Event::new(EventKind::Custom("x".into()), None);
    }
}
