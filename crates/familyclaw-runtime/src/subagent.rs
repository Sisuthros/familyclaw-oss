//! Väliaikaisen apuagentin spawneri jaettuun resonance-busiin.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use familyclaw_actions::{SpawnSubagentSkill, SubagentSpawner};
use familyclaw_agent::{
    build_llm_chain, new_reply_channel, Agent, ErasedMemoryStore, LlmEndpointResolver, Soul,
};
use familyclaw_bus::{BusHandle, BusMessage, MessageOrigin};
use familyclaw_core::{AgentConfig, ModelConfig};
use familyclaw_durable::{DurableContext, InMemoryJournal, Journal};
use familyclaw_embeddings::DeterministicEmbedder;
use familyclaw_memory::{EmbeddingMemoryStore, LocalJsonStore};
use tokio::time::timeout;

/// Runtime-toteutus: spawnaa kevyen apuagentin busille, lähettää tehtävän ja
/// odottaa vastauksen reply-sinkistä.
pub struct BusSubagentSpawner {
    bus: BusHandle,
    model: ModelConfig,
    resolver: Arc<dyn LlmEndpointResolver + Send + Sync>,
    default_reply_target: String,
}

impl BusSubagentSpawner {
    /// Luo spawnerin, joka delegoi apuagentit busille annetulla mallilla,
    /// LLM-resolverilla ja oletus-reply-kohteella.
    #[must_use]
    pub fn new(
        bus: BusHandle,
        model: ModelConfig,
        resolver: Arc<dyn LlmEndpointResolver + Send + Sync>,
        default_reply_target: impl Into<String>,
    ) -> Self {
        Self {
            bus,
            model,
            resolver,
            default_reply_target: default_reply_target.into(),
        }
    }
}

#[async_trait]
impl SubagentSpawner for BusSubagentSpawner {
    async fn spawn_and_run(
        &self,
        task: &str,
        helper_name: Option<&str>,
    ) -> std::result::Result<String, String> {
        let name = helper_name.unwrap_or("helper_agent");
        let agent_cfg = AgentConfig::new_with_stable_id(name, self.model.clone());
        let soul = Soul::from_essence(format!(
            "I am {name}, a temporary helper agent on the FamilyClaw bus."
        ));

        let journal: Arc<dyn Journal + Send + Sync> = Arc::new(InMemoryJournal::new());
        let durable =
            DurableContext::new(Arc::clone(&journal)).map_err(|e| format!("durable init: {e}"))?;
        let mem: ErasedMemoryStore = Arc::new(EmbeddingMemoryStore::new(
            LocalJsonStore::in_memory(),
            Arc::new(DeterministicEmbedder::new()),
        ));

        let failover = build_llm_chain(&agent_cfg.model, self.resolver.as_ref())
            .map_err(|e| format!("llm chain: {e}"))?;

        let (sink, mut reply_rx) = new_reply_channel();
        let reply_target = self.default_reply_target.clone();

        let agent = Agent::new(agent_cfg, soul, mem, durable, self.bus.clone(), None, None)
            .with_failover(failover)
            .with_reply_sink(sink)
            .with_reply_target(reply_target.clone());

        let actor = agent
            .spawn()
            .await
            .map_err(|e| format!("spawn helper: {e}"))?;

        let origin = MessageOrigin::new("subagent", &reply_target, "operator");
        let sender = familyclaw_bus::BeingId::new();
        self.bus
            .publish_with_origin(sender, BusMessage::text(task), origin)
            .map_err(|e| format!("publish task: {e}"))?;

        let reply = timeout(Duration::from_secs(120), reply_rx.recv())
            .await
            .map_err(|_| "subagent timed out waiting for reply".to_string())?
            .ok_or_else(|| "subagent reply channel closed".to_string())?
            .body;

        drop(actor);
        Ok(reply)
    }
}

/// Rekisteröi [`SpawnSubagentSkill`] annettuun toimintoajoympäristöön.
pub fn register_spawn_subagent_skill(
    runtime: &mut familyclaw_actions::ActionRuntime,
    spawner: Arc<dyn SubagentSpawner>,
) -> familyclaw_core::Result<()> {
    runtime
        .register_skill(SpawnSubagentSkill::new(spawner))
        .map_err(|e| familyclaw_core::FamilyClawError::config(format!("spawn_subagent: {e}")))
}
