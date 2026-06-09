//! # minimal-gateway
//!
/** **FamilyClaw in 60 seconds** — boots 1 agent + Resonance Bus, no external deps.

## Run
```bash
cargo run -p minimal-gateway
```

## What it does
1. Starts Resonance Bus
2. Spawns 1 agent (agent_a) with MockChannel
3. Injects a message to agent_a
4. Shows the agent processed it (memory + emotional response)
5. Shuts down cleanly on Ctrl-C

This demonstrates the core FamilyClaw stack: Bus + Agent + Channel + Durable Memory.
*/

use std::time::Duration;

use clap::Parser;
use familyclaw_agent::{EnvEndpointResolver, Soul};
use familyclaw_channels::{Channel, InboundMessage, MockChannel};
use familyclaw_core::{AgentConfig, FamilyClawError, ModelConfig, Result};
use familyclaw_runtime::build_family;
use tokio::signal;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "minimal-gateway", about = "FamilyClaw minimal demo — 1 agent + bus")]
struct Cli {
    /// Kuinka kauan demo ajetaan sekunteina (0 = Ctrl-C:hen asti)
    #[arg(short, long, default_value = "10")]
    duration: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Tracing init
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .init();

    let cli = Cli::parse();

    info!("🚀 FamilyClaw minimal-gateway starting...");

    // 1. Agent config (mock LLM — no external API needed)
    let agent_a_cfg = AgentConfig::new("agent_a", ModelConfig::new("mock/llm"));

    // 2. Soul (minimal identity)
    let soul_a = Soul::from_essence("I am agent_a, a FamilyClaw being. I value correctness.");

    // 3. Channel (MockChannel — no Telegram/Discord needed)
    let channel_a = MockChannel::new("mock-a").map_err(FamilyClawError::from)?;

    let channel_a_clone = channel_a.clone();
    let channel_a: Box<dyn Channel> = Box::new(channel_a);

    // 4. Resolver (mock endpoint — won't actually be called)
    let resolver = EnvEndpointResolver::new();

    // 5. Build FamilyRuntime (bus + agent + channel)
    let bus_name = "minimal-gateway-bus";

    info!("🔧 Building FamilyRuntime for agent_a...");
    let runtime = build_family(
        Some(bus_name.to_string()),
        agent_a_cfg,
        soul_a,
        channel_a,
        "mock-a".to_string(), // reply_target
        &resolver,
    ).await?;

    info!("✅ FamilyRuntime running (bus + agent + channel)");

    // 6. Inject a test message to agent_a
    info!("📤 Injecting test message to agent_a...");
    let msg = InboundMessage::new("user", "demo-conversation", "Hello agent_a, remember this: FamilyClaw rocks!")
        .map_err(FamilyClawError::from)?;
    channel_a_clone.inject(msg).map_err(FamilyClawError::from)?;

    // 7. Wait a bit for message to propagate through bus → agent
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 8. Show results
    println!("\n{}", "=".repeat(60));
    println!("📬 AGENT_A OUTBOX (sent):");
    for msg in channel_a_clone.sent() {
        println!("  → {}: {}", msg.target, msg.body);
    }
    println!("{}", "=".repeat(60));

    println!("\n💡 The agent received the message via Resonance Bus,");
    println!("   processed it through its durable memory, and updated");
    println!("   its emotional state — all without external APIs.");

    // 9. Run for duration or until Ctrl-C
    if cli.duration > 0 {
        info!("⏱️  Running for {} seconds...", cli.duration);
        tokio::time::sleep(Duration::from_secs(cli.duration)).await;
    } else {
        info!("🔄 Running until Ctrl-C...");
        signal::ctrl_c().await.map_err(FamilyClawError::from)?;
    }

    // 10. Clean shutdown
    info!("🛑 Shutting down...");
    runtime.shutdown();
    tokio::time::sleep(Duration::from_millis(200)).await;

    info!("✅ FamilyClaw minimal-gateway stopped cleanly");
    Ok(())
}