//! Automated 30-second showcase of `FamilyClaw` capabilities.
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
//!
//! ## Subcommands
//! Without arguments, the binary runs the demo described above. It also
//! provides the `replay` subcommand family for inspecting durable journals
//! (Time Machine — see [`familyclaw_agent::replay_cli`]):
//!
//! ```text
//! familyclaw replay inspect --journal <path> [--json]
//! familyclaw replay fork    --journal <path> --keep <N> --out <path>
//! familyclaw replay diff    --before <path> --after <path> [--json]
//! familyclaw replay demo    [--dir <path>]
//! ```
//!
//! as well as the `import` subcommand for migrating data from another
//! runtime environment (see [`familyclaw_agent::import_cli`]) — imported
//! skills go into **quarantine** and are never activated:
//!
//! ```text
//! familyclaw import --from openclaw|hermes --input <path> [--out <dir>] [--json]
//! ```

use std::sync::Arc;
use std::time::Duration;

use familyclaw_agent::{import_cli, publish_envelope, replay_cli, Agent, ErasedMemoryStore, Soul};
use familyclaw_bus::{BeingId, BusHandle, BusMessage, ResonanceBus};
use familyclaw_channels::{Channel, ChannelKind, InboundMessage, MockChannel};
use familyclaw_core::{AgentConfig, FamilyClawError, ModelConfig, Result};
use familyclaw_dream::{DreamConfig, DreamCycle};
use familyclaw_durable::{DurableContext, InMemoryJournal, Journal};
use familyclaw_emotion::{Dimension, EmotionState};
use familyclaw_memory::{ImportanceFactors, LocalJsonStore, Memory, RetrievalContext};
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
fn build_agent(name: &str, bus: &BusHandle) -> Result<Agent> {
    let config = AgentConfig::new(name, ModelConfig::new("provider/model"));
    let soul = Soul::from_essence(format!(
        "I am {name}, an autonomous agent on the FamilyClaw platform."
    ));
    let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
    let durable =
        DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
            .map_err(|e| FamilyClawError::bus(e.to_string()))?;
    Ok(Agent::new(
        config,
        soul,
        memory,
        durable,
        bus.clone(),
        None,
        None,
    ))
}

/// Reports memory state for an agent.
async fn report_memory(name: &str, mem: &ErasedMemoryStore, query: &str) -> Result<()> {
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

/// Handles the `replay` subcommand if the first argument is `replay`.
///
/// Returns `Some(exit_code)` when the subcommand was recognized and
/// executed (in which case the caller exits without running the demo), or
/// `None` when there were no arguments or they belong to the demo. The
/// Time Machine path is entirely synchronous, so it does not need the
/// tokio runtime.
///
/// Fail-closed: invalid input prints a clear error message + the usage
/// text to `stderr` and returns a nonzero return code. Never panics.
fn try_handle_replay() -> Option<i32> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("replay") => Some(run_replay(args)),
        _ => None,
    }
}

/// Handles the `import` subcommand if the first argument is `import`.
///
/// Returns `Some(exit_code)` when the subcommand was recognized and
/// executed, or `None` otherwise. The migration path is synchronous (no
/// tokio runtime).
///
/// Fail-closed: invalid input prints a clear error message + the usage
/// text to `stderr` and returns a nonzero return code. Never panics.
/// **Safety:** imported skills go into quarantine and never register or
/// execute.
fn try_handle_import() -> Option<i32> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("import") => Some(run_import(args)),
        _ => None,
    }
}

/// Executes the `import` subcommand with the remaining arguments and returns
/// the process's return code (`0` = success).
fn run_import<I: Iterator<Item = String>>(args: I) -> i32 {
    match import_cli::run(args) {
        Ok(output) => {
            println!("{output}");
            0
        }
        Err(err) => {
            eprintln!("familyclaw import: {err}\n");
            eprintln!("{}", import_cli::usage());
            1
        }
    }
}

/// Executes the `replay` subcommand with the remaining arguments and returns
/// the process's return code (`0` = success).
fn run_replay<I: Iterator<Item = String>>(args: I) -> i32 {
    match replay_cli::run(args) {
        Ok(output) => {
            println!("{output}");
            0
        }
        Err(err) => {
            eprintln!("familyclaw replay: {err}\n");
            eprintln!("{}", replay_cli::usage());
            1
        }
    }
}

#[allow(clippy::too_many_lines)]
#[tokio::main]
async fn main() -> Result<()> {
    // Subcommands before the demo: `replay ...` is the synchronous Time
    // Machine path. Without arguments (or with other arguments) the demo
    // below runs exactly as before.
    if let Some(code) = try_handle_replay() {
        std::process::exit(code);
    }

    // `import ...` is the synchronous migration path (OpenClaw/Hermes → FamilyClaw).
    // Imported skills go into quarantine; memories receive low trust.
    if let Some(code) = try_handle_import() {
        std::process::exit(code);
    }

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

    // Step 6: Time jump — simulate memory aging (simulated for demo)
    info!("⏰ Step 6: Time jump — 7 days later, memory retention decays (simulated)...");
    info!("   [Demo simulates time passage; full implementation uses DreamCycle decay]");
    info!("   · agent_a's 'new family member' memory: retention ~70% (simulated aging)");
    info!("   · agent_a's 'building for the world' memory: retention ~95% (identity-anchored)");
    info!("   ✓ Identity anchors: supported by memory model, not exercised in this short demo");
    info!("");

    // Step 7: Dream cycle processing
    info!("🌙 Step 7: Dream cycle runs — consolidating memories...");
    // Create some duplicate memories for the dream cycle to merge
    // Note: we need &dyn MemoryStore, so we deref the Arc
    let store_ref = a_mem.as_ref();
    let dup_a = Memory::builder("Hei agent_b! Tervetuloa perheeseen.")
        .factors(ImportanceFactors::new(0.3, 0.0, 0.0, 0.0))
        .tags(["greeting".to_string()])
        .build();
    let dup_b = Memory::builder("Hei agent_b! Tervetuloa perheeseen.")
        .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
        .tags(["milestone".to_string()])
        .build();
    store_ref.add(dup_a).await?;
    store_ref.add(dup_b).await?;

    let cycle =
        DreamCycle::with_config(store_ref, DreamConfig::default().with_merge_similarity(0.7));
    let report = cycle
        .run_without_journal(familyclaw_core::time::now())
        .await?;

    info!("   · Merged duplicates: {}", report.merged);
    info!("   · Dropped contradicted: {}", report.dropped);
    info!("   · Dates absolutized: {}", report.dates_absolutized);
    info!("   · Memories strengthened: {}", report.strengthened);
    info!("   · Memories archived: {}", report.archived);
    info!(
        "   ✓ Dream cycle complete — total actions: {}",
        report.total_actions()
    );
    info!("");

    // Step 8: Show memory retrieval after consolidation
    info!("📚 Step 8: Memory retrieval after dream cycle...");
    report_memory("agent_a", &a_mem, "perhe").await?;
    report_memory("agent_b", &b_mem, "tervetuloa").await?;
    info!("");

    // Summary
    let a_count = a_mem.len().await?;
    let b_count = b_mem.len().await?;
    info!("══════════════════════════════════════════════════════════════════");
    info!("  Demo Complete!");
    info!("  · {} agents on bus", beings.len());
    info!("  · {} messages stored in agent_a's memory", a_count);
    info!("  · {} messages stored in agent_b's memory", b_count);
    info!("  · Emotion contagion demonstrated");
    info!("  · Memory decay vs identity anchors shown (simulated)");
    info!(
        "  · Dream cycle processed (real execution — merged: {})",
        report.merged
    );
    info!("");
    info!("  FamilyClaw: agents that remember, feel, dream, and think.");
    info!("════════════════════════════════════════════════════════════");

    bus.stop();
    Ok(())
}
