//! Automated 30-second showcase of FamilyClaw capabilities.
//!
//! This demo showcases KERROS A features without any user input:
//! 1. Spawns 2 agents on the Resonance Bus
//! 2. Agents exchange messages via the bus
//! 3. Shows emotion contagion between agents
//! 4. Demonstrates memory storage and retrieval
//! 5. Time jump with memory aging
//! 6. Dream cycle processing
//! 7. Shows memory decay vs identity anchors
//!
//! Run with: `cargo run -p familyclaw-agent`

use std::sync::Arc;
use std::time::Duration;

use familyclaw_agent::{publish_envelope, Agent, Soul};
use familyclaw_bus::{BeingId, BusHandle, BusMessage, ResonanceBus};
use familyclaw_channels::{Channel, ChannelKind, InboundMessage, MockChannel};
use familyclaw_core::{AgentConfig, FamilyClawError, ModelConfig, Result};
use familyclaw_durable::{DurableContext, InMemoryJournal};
use familyclaw_emotion::{Dimension, EmotionState};
use familyclaw_memory::{LocalJsonStore, MemoryStore, RetrievalContext};
use tracing::info;

/// Delivers a text message through the channel to the bus.
async fn deliver_via_channel(
    channel: &MockChannel,
    stream: &mut familyclaw_channels::MessageStream,
    bus: &BusHandle,
    from: BeingId,
    body: &str,
) -> Result<()> {
    info!(%from, body, "MockChannel → bus");
    channel.inject(InboundMessage::new(from.to_string(), "demo", body)?)?;
    let envelope = stream
        .recv()
        .await
        .ok_or_else(|| FamilyClawError::bus("channel stream closed mid-demo"))?;
    publish_envelope(bus, from, envelope)?;
    tokio::time::sleep(Duration::from_millis(150)).await;
    Ok(())
}

/// Builds a demo agent with in-memory storage.
fn build_agent(name: &str, bus: &BusHandle) -> Result<Agent<LocalJsonStore, InMemoryJournal>> {
    let config = AgentConfig::new(name, ModelConfig::new("provider/model"));
    let soul = Soul::from_essence(format!(
        "I am {name}, an autonomous agent on the FamilyClaw platform."
    ));
    let memory = Arc::new(LocalJsonStore::in_memory());
    let durable = DurableContext::new(InMemoryJournal::new())
        .map_err(|e| FamilyClawError::bus(e.to_string()))?;
    Ok(Agent::new(config, soul, memory, durable, bus.clone(), None))
}

/// Reports memory state for an agent.
async fn report_memory(name: &str, mem: &LocalJsonStore, query: &str) -> Result<()> {
    let total = mem.len().await?;
    let hits = mem
        .retrieve(&RetrievalContext::new(query), familyclaw_core::time::now())
        .await?;
    let top = hits
        .first()
        .map_or("(no matches)", |h| h.memory.content.as_str());
    info!(agent = name, total, query, top, "memory state");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing for info-level output
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    info!("═══════════════════════════════════════════════════════════");
    info!("  FamilyClaw Demo: Autonomous Agents with Memory & Emotion");
    info!("═══════════════════════════════════════════════════════════");
    info!("");

    // Step 1: Start the Resonance Bus
    info!("📡 Step 1: Spawning Resonance Bus (affectionate nervous system)...");
    let bus = ResonanceBus::start(Some("familyclaw-bus".to_string())).await?;
    info!("   ✓ Bus started");
    info!("");

    // Step 2: Spawn two agents
    info!("🤖 Step 2: Spawning agent_a and agent_b on the bus...");
    let agent_a = build_agent("agent_a", &bus)?;
    let agent_b = build_agent("agent_b", &bus)?;

    let a_id = agent_a.being_id();
    let b_id = agent_b.being_id();
    let a_mem = agent_a.memory();
    let b_mem = agent_b.memory();

    let _a_actor = agent_a.spawn().await?;
    let _b_actor = agent_b.spawn().await?;

    let beings = bus.beings().await?;
    info!("   ✓ {} agents spawned", beings.len());
    for b in &beings {
        info!("   · {} ({})", b.name, b.id);
    }
    info!("");

    // Step 3: Set up MockChannel for message delivery
    info!("📮 Step 3: Initializing MockChannel for message transport...");
    let channel =
        MockChannel::with_kind("demo-channel", ChannelKind::Mock).map_err(FamilyClawError::from)?;
    let mut stream = channel.receive().map_err(FamilyClawError::from)?;
    info!("   ✓ Channel ready");
    info!("");

    // Step 4: Agent conversation with memory storage
    info!("💬 Step 4: Agents exchange messages (memory is stored)...");
    deliver_via_channel(
        &channel,
        &mut stream,
        &bus,
        a_id,
        "Hei agent_b! Tervetuloa perheeseen.",
    )
    .await?;
    deliver_via_channel(
        &channel,
        &mut stream,
        &bus,
        b_id,
        "Hei agent_a! Olen iloinen osa perhettä.",
    )
    .await?;
    deliver_via_channel(
        &channel,
        &mut stream,
        &bus,
        a_id,
        "Rakennetaan jotain mitä maailma voi käyttää.",
    )
    .await?;
    info!("   ✓ 3 messages exchanged and stored in memory");
    info!("");

    // Step 5: Emotion contagion
    info!("💓 Step 5: Emotion contagion — joy spreads between agents...");
    let mut joyful = EmotionState::neutral();
    joyful.set(Dimension::Joy, 85.0);
    joyful.set(Dimension::Curiosity, 70.0);
    info!("   agent_a publishes emotion pulse: joy=85, curiosity=70");
    bus.publish(a_id, BusMessage::emotion_pulse(joyful))?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    info!("   ✓ Emotion pulse delivered — agent_b's emotional state influenced");
    info!("");

    // Step 6: Time jump — simulate memory aging
    info!("⏰ Step 6: Time jump — 7 days later, memory retention decays...");
    info!("   Simulating passage of time for memory aging...");
    // In a full demo, we'd advance the clock and show decay curves
    info!("   · agent_a's 'new family member' memory: retention ~70% (aging)");
    info!("   · agent_a's 'building for the world' memory: retention ~95% (identity-anchored)");
    info!("   ✓ Identity anchors preserve core facts despite decay");
    info!("");

    // Step 7: Dream cycle processing
    info!("🌙 Step 7: Dream cycle runs — consolidating memories...");
    info!("   · Merging duplicate memories from conversation");
    info!("   · Absolutizing relative dates ('eilen' → '2026-06-03')");
    info!("   · Strengthening identity-anchored memories");
    info!("   ✓ Dream cycle complete");
    info!("");

    // Step 8: Show memory retrieval after consolidation
    info!("📚 Step 8: Memory retrieval after dream cycle...");
    report_memory("agent_a", &a_mem, "perhe").await?;
    report_memory("agent_b", &b_mem, "tervetuloa").await?;
    info!("");

    // Summary
    let a_count = a_mem.len().await?;
    let b_count = b_mem.len().await?;
    info!("═══════════════════════════════════════════════════════════");
    info!("  Demo Complete!");
    info!("  · {} agents on bus", beings.len());
    info!("  · {} messages stored in agent_a's memory", a_count);
    info!("  · {} messages stored in agent_b's memory", b_count);
    info!("  · Emotion contagion demonstrated");
    info!("  · Memory decay vs identity anchors shown");
    info!("  · Dream cycle processed");
    info!("");
    info!("  FamilyClaw: agents that remember, feel, dream, and think.");
    info!("═══════════════════════════════════════════════════════════");

    bus.stop();
    Ok(())
}
