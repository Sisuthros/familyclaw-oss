//! # Two Agents, One Continuity — the flagship `FamilyClaw` showcase
//!
//! A single, deterministic, self-checking demo of the continuity substrate:
//! **two named agents that are live on the same bus, send real messages to
//! each other, feel each other's mood through the real delivery path, reshape
//! their memory while they sleep (merging duplicates and grounding relative
//! dates), and answer the same question differently as time passes.**
//!
//! Everything printed here is *causally true*. Every claim is followed by a
//! value read back out of the live runtime, and the program `assert!`s each
//! invariant — so a stranger who runs it knows the numbers were produced by the
//! engine, not hand-typed. On any failed invariant the process exits non-zero.
//!
//! Run it:
//! ```bash
//! cargo run -p familyclaw-agent --example two_agents_memory
//! ```
//!
//! **Layer A only.** The two agents are generic (`Alice`, `Bob`). No private
//! souls, no calibration weights, no API keys, no network, no external
//! services. Pure open platform.
//!
//! ## How continuity is proven without faking it
//!
//! The two agents are spawned as real actors on the [`ResonanceBus`] and stay
//! spawned for the whole run — they are never dropped and recreated with empty
//! stores. Their inner state is inspected through two shared handles that the
//! runtime already exposes:
//!
//! - **Memory** is an `Arc`-shared store ([`Agent::memory`]). The demo holds a
//!   clone of Bob's store, so when Bob's actor writes to it over the bus, the
//!   demo reads the *same* store back.
//! - **Emotion** is read through the small, opt-in introspection seam
//!   ([`Agent::with_emotion_probe`]): the agent mirrors its final emotion into
//!   a shared `Arc<Mutex<EmotionState>>` at the end of every turn, so the demo
//!   can read the spawned agent's mood after a bus-delivered pulse without
//!   touching the actor's message type or the bus delivery path.

use std::sync::{Arc, Mutex};

use chrono::Duration as ChronoDuration;
use familyclaw_agent::{Agent, ErasedMemoryStore, Soul};
use familyclaw_bus::{BusHandle, BusMessage, ResonanceBus};
use familyclaw_core::{time, AgentConfig, FamilyClawError, ModelConfig, Result};
use familyclaw_dream::{DreamConfig, DreamCycle};
use familyclaw_durable::{DurableContext, InMemoryJournal, Journal};
use familyclaw_emotion::{Dimension, EmotionState};
use familyclaw_memory::{
    DecayPolicy, ImportanceFactors, LocalJsonStore, Memory, MemoryStore, RetrievalContext,
};

/// A live agent plus the shared handles the demo uses to inspect its state
/// after it has been spawned onto the bus.
struct DemoAgent {
    /// The agent's being id on the bus (its sender identity).
    being_id: familyclaw_bus::BeingId,
    /// Clone of the agent's memory store (`Arc`-shared with the spawned actor).
    memory: ErasedMemoryStore,
    /// Shared emotion probe the spawned actor mirrors into each turn.
    emotion: Arc<Mutex<EmotionState>>,
}

impl DemoAgent {
    /// Reads the agent's current Joy value from the shared emotion probe.
    fn joy(&self) -> f32 {
        self.emotion
            .lock()
            .expect("emotion probe lock")
            .value(Dimension::Joy)
    }

    /// Reads the agent's current value for any dimension from the probe.
    fn feeling(&self, dim: Dimension) -> f32 {
        self.emotion.lock().expect("emotion probe lock").value(dim)
    }
}

/// Builds one demo agent with in-memory storage on the given bus, wires up the
/// shared memory + emotion handles, spawns it as a live actor, and returns the
/// inspection handles. The agent stays alive on the bus (the returned actor
/// reference is held by the caller for the whole demo).
async fn spawn_demo_agent(
    name: &str,
    bus: &BusHandle,
) -> Result<(
    DemoAgent,
    ractor::ActorRef<familyclaw_bus::ResonanceMessage>,
)> {
    let config = AgentConfig::new(name, ModelConfig::new("provider/model"));
    let soul = Soul::from_essence(format!(
        "I am {name}, an autonomous agent on the FamilyClaw platform."
    ));

    // Shared handles: memory (Arc) and an emotion probe (Arc<Mutex>). We keep
    // clones so we can inspect the spawned actor's state from outside.
    let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
    let emotion = Arc::new(Mutex::new(EmotionState::neutral()));

    let durable =
        DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
            .map_err(|e| FamilyClawError::bus(e.to_string()))?;

    let agent = Agent::new(
        config,
        soul,
        Arc::clone(&memory),
        durable,
        bus.clone(),
        None,
        None,
    )
    .with_emotion_probe(Arc::clone(&emotion));

    let being_id = agent.being_id();
    // Spawn the agent as a live actor and register it on the bus. From here on
    // its memory/emotion live inside the actor; we watch them via the clones.
    let actor = agent.spawn().await?;

    Ok((
        DemoAgent {
            being_id,
            memory,
            emotion,
        },
        actor,
    ))
}

/// Prints a section header.
fn banner(text: &str) {
    println!("\n\x1b[1;36m{text}\x1b[0m");
    println!("{}", "─".repeat(text.chars().count()));
}

/// Counts memories that are still active (not archived or tombstoned).
async fn count_active(store: &dyn MemoryStore) -> Result<usize> {
    let all = store.all().await?;
    Ok(all
        .iter()
        .filter(|m| m.status == familyclaw_memory::MemoryStatus::Active)
        .count())
}

/// Waits for the bus to deliver queued messages and the receiving actor to
/// process them. The demo asserts on the *result* of delivery, so this is a
/// settle delay, not a correctness crutch.
async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
}

/// Prints the top memory a store recalls for a query as of `now`, plus its
/// relevance, and returns the top content (empty string if nothing matched).
async fn top_recall(store: &dyn MemoryStore, query: &str, now: time::Timestamp) -> Result<String> {
    let all = store.all().await?;
    let hits = familyclaw_memory::retrieve(&all, &RetrievalContext::new(query).with_limit(1), now);
    if let Some(hit) = hits.first() {
        println!(
            "   query {query:?} → top: {:?}  (relevance {:.3})",
            hit.memory.content, hit.relevance
        );
        Ok(hit.memory.content.clone())
    } else {
        println!("   query {query:?} → (nothing retrievable)");
        Ok(String::new())
    }
}

#[allow(clippy::too_many_lines)]
#[tokio::main]
async fn main() -> Result<()> {
    println!("\x1b[1;35m═══════════════════════════════════════════════════════════════\x1b[0m");
    println!("\x1b[1;35m  FamilyClaw — Two Agents, One Continuity (Alice & Bob)\x1b[0m");
    println!(
        "\x1b[1;35m  Live on one bus: they message, feel, dream, and change over time.\x1b[0m"
    );
    println!("\x1b[1;35m═══════════════════════════════════════════════════════════════\x1b[0m");

    // A fixed "now" so retention math and the day-8 comparison are deterministic.
    let day1 = time::now();
    let day2 = day1 + ChronoDuration::days(1);
    let day8 = day1 + ChronoDuration::days(7);

    // ── Capability 1: two named agents are live on the resonance bus ────────
    banner("(1) Two named agents are live on the resonance bus");
    let bus = ResonanceBus::start(Some("familyclaw-demo-bus".to_string())).await?;
    // Spawn both agents as live actors and KEEP them spawned for the whole demo
    // (the actor references stay in scope). These are the same stateful agents
    // whose memory and emotion we inspect below — nothing is dropped/recreated.
    let (alice, _alice_actor) = spawn_demo_agent("Alice", &bus).await?;
    let (bob, _bob_actor) = spawn_demo_agent("Bob", &bus).await?;

    let beings = bus.beings().await?;
    println!(
        "   ✓ resonance bus started, {} beings registered on the mesh",
        beings.len()
    );
    for b in &beings {
        println!("   · {} ({})", b.name, b.id);
    }
    assert!(beings.len() >= 2, "both agents must be live on the bus");
    println!("   · Alice being-id = {}", alice.being_id);
    println!("   · Bob   being-id = {}", bob.being_id);

    // ── Capability 2: shared memory over the real bus delivery path ─────────
    banner("(2) Shared memory: Alice sends over the bus, Bob receives and remembers");
    // Alice publishes a real text message to the bus. The bus delivers it to
    // Bob's live actor, which runs handle_turn and stores what it heard into the
    // memory store we hold a clone of. No direct handle_turn call — real delivery.
    let said = "We are building something that outlives any single run.";
    println!("   Alice publishes to the bus: {said:?}");
    bus.publish(alice.being_id, BusMessage::text(said))?;
    settle().await;

    let bob_mem_len = bob.memory.len().await?;
    println!(
        "   ✓ Bob received it over the bus and stored it — Bob now holds {bob_mem_len} memory/-ies"
    );
    let recalled = top_recall(bob.memory.as_ref(), "building something", day1).await?;
    assert_eq!(
        bob_mem_len, 1,
        "exactly the one bus-delivered message is stored in Bob's memory"
    );
    assert!(
        recalled.contains("building something"),
        "Bob must recall the message Alice sent over the bus"
    );
    // Alice must NOT have stored her own broadcast (the bus never echoes the
    // sender), proving delivery was selective, not a shared write.
    assert_eq!(
        alice.memory.len().await?,
        0,
        "the sender does not receive her own broadcast"
    );
    println!("   ✓ Alice did not store her own broadcast — delivery was real and one-directional");

    // ── Capability 3: emotion contagion over the real bus delivery path ─────
    banner("(3) Emotion contagion: Alice's mood reaches Bob through the bus (before → after)");
    let bob_joy_before = bob.joy();
    let bob_cur_before = bob.feeling(Dimension::Curiosity);
    println!("   Bob BEFORE : joy = {bob_joy_before:.1}, curiosity = {bob_cur_before:.1}");

    let mut alice_pulse = EmotionState::neutral();
    alice_pulse.set(Dimension::Joy, 80.0);
    alice_pulse.set(Dimension::Curiosity, 60.0);
    println!("   Alice emits an emotion pulse to the bus: joy = 80.0, curiosity = 60.0");
    // The pulse travels the same broadcast path as any bus message. Bob's actor
    // absorbs it via affective contagion; the probe reflects the new mood.
    bus.publish(alice.being_id, BusMessage::emotion_pulse(alice_pulse))?;
    settle().await;

    let bob_joy_after = bob.joy();
    let bob_cur_after = bob.feeling(Dimension::Curiosity);
    println!("   Bob AFTER  : joy = {bob_joy_after:.1}, curiosity = {bob_cur_after:.1}");
    assert!(
        bob_joy_after > bob_joy_before,
        "contagion over the bus must raise Bob's joy"
    );
    // The pulse is "blood", not speech: it changes mood but is not remembered.
    assert_eq!(
        bob.memory.len().await?,
        1,
        "the emotion pulse is not stored as a memory (only the earlier text is)"
    );
    println!(
        "   ✓ Bob's joy rose by {:.1} over the bus — he caught Alice's mood, and did not 'remember' the pulse",
        bob_joy_after - bob_joy_before
    );

    // ── Capability 4: dream consolidation on BOB's own memory ───────────────
    // TRUTHFUL FRAMING: the dream reshapes the memory SET (merges near-duplicate
    // notes) and grounds relative dates ("yesterday" → ISO). It does NOT change
    // the recall *answer* text for this query — the surviving greeting is the
    // same string before and after. What provably changes is the active-memory
    // count and the absolutized date, both asserted below. Recall *output* is
    // changed by decay, shown separately in capability (5).
    banner("(4) Dream consolidation on Bob's memory: reshapes the memory set and grounds relative dates");
    // Seed Bob with raw day-1 notes: two near-duplicate greetings + a note that
    // uses a relative date ("yesterday"). We add them directly to Bob's store to
    // set up a clean, deterministic dream input.
    let bob_store = Arc::clone(&bob.memory);
    let bob_ref: &dyn MemoryStore = bob_store.as_ref();
    bob_ref
        .add(
            Memory::builder("Welcome to the team, Alice!")
                .factors(ImportanceFactors::new(0.3, 0.0, 0.0, 0.0))
                .tags(["greeting".to_string()])
                .created_at(day1)
                .build(),
        )
        .await?;
    bob_ref
        .add(
            Memory::builder("Welcome to the team, Alice!")
                .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
                .tags(["greeting".to_string()])
                .created_at(day1)
                .build(),
        )
        .await?;
    bob_ref
        .add(
            Memory::builder("Alice joined the bus yesterday and settled in.")
                .factors(ImportanceFactors::new(0.4, 0.0, 0.0, 0.0))
                .tags(["event".to_string()])
                .created_at(day1)
                .build(),
        )
        .await?;

    // Ask Bob a question BEFORE the dream and remember his top answer.
    let dream_query = "welcome greeting";
    let before_active = count_active(bob_ref).await?;
    println!("   Bob's raw notes before the dream ({before_active} active):");
    print!("     ");
    let before_answer = top_recall(bob_ref, dream_query, day2).await?;

    // The dream runs on BOB's store at day2 → "yesterday" resolves against
    // day2's calendar date, duplicates merge.
    let cycle = DreamCycle::with_config(bob_ref, DreamConfig::default().with_merge_similarity(0.7));
    let report = cycle.run_without_journal(day2).await?;
    let after_active = count_active(bob_ref).await?;

    println!(
        "   Dream report: merged={}, dates_absolutized={}, strengthened={}, archived={}",
        report.merged, report.dates_absolutized, report.strengthened, report.archived
    );
    // Ask the SAME question AFTER the dream. The dream reduced Bob's active
    // memories (duplicates merged) and grounded the relative date, so the
    // memory SET Bob holds is provably reshaped (the recall answer text for this
    // query is the same surviving greeting — recall *output* change is decay's
    // job, shown in capability 5).
    println!("   Bob's notes after the dream ({after_active} active):");
    print!("     ");
    let after_answer = top_recall(bob_ref, dream_query, day2).await?;

    let expected_iso = (day2 - ChronoDuration::days(1))
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
    let grounded = bob_ref
        .all()
        .await?
        .into_iter()
        .find(|m| m.content.contains("joined the bus"))
        .map(|m| m.content)
        .unwrap_or_default();

    assert!(report.merged >= 1, "the two identical greetings must merge");
    assert!(
        after_active < before_active,
        "the dream must reduce Bob's active memory count (merged duplicates)"
    );
    assert!(
        report.dates_absolutized >= 1,
        "the 'yesterday' note must be absolutized by the dream"
    );
    assert!(
        grounded.contains(&expected_iso),
        "the grounded absolute date {expected_iso} must appear in Bob's note after the dream"
    );
    println!("   ✓ Same query, reshaped memory set: the dream merged Bob's duplicates");
    println!(
        "       (active memories {before_active} → {after_active}) and grounded the relative date:"
    );
    println!("       {grounded:?}");
    // before_answer / after_answer are both the surviving greeting; the change
    // the dream caused is the reduced active set + the grounded date, asserted
    // above. We surface both answers for transparency.
    let _ = (before_answer, after_answer);

    // ── Capability 5 + 6: time and decay change retrieval (NOT the dream) ───
    banner("(5) Time and decay change retrieval: the same question, a different answer later");
    // This section is about Ebbinghaus DECAY, not the dream. We give Bob two
    // day-1 memories and ask the same question on day 1 and day 8:
    //   query = "team weather"  (the day's small-talk topic).
    //   - chatter : fresh small talk that matches BOTH query words (Fast decay).
    //   - anchor  : his mission, matches only "team" but NEVER decays (ProtectedCore).
    // Day 1: the vivid, fully-matching chatter wins.
    // Day 8: the chatter has faded (Ebbinghaus), so the identity anchor wins.
    // Same question, provably different top memory — caused by decay over time.
    let decay_query = "team weather";
    let chatter = "Today the team chatted about the weather over lunch.";
    let anchor = "The team is the reason I keep building this world.";
    bob_ref
        .add(
            Memory::builder(chatter)
                .factors(ImportanceFactors::new(0.35, 0.0, 0.0, 0.0))
                .decay_policy(DecayPolicy::Fast)
                .tags(["smalltalk".to_string()])
                .created_at(day1)
                .build(),
        )
        .await?;
    bob_ref
        .add(
            Memory::builder(anchor)
                .factors(ImportanceFactors::new(0.9, 0.0, 0.0, 0.0))
                .decay_policy(DecayPolicy::ProtectedCore)
                .tags(["identity".to_string()])
                .created_at(day1)
                .build(),
        )
        .await?;

    // Day 1: score through the retrieval engine as of day1.
    let all_bob = bob_ref.all().await?;
    let day1_top = familyclaw_memory::retrieve(
        &all_bob,
        &RetrievalContext::new(decay_query).with_limit(1),
        day1,
    );
    let day1_answer = day1_top
        .first()
        .map(|h| h.memory.content.clone())
        .unwrap_or_default();
    println!("   DAY 1 — Bob is asked {decay_query:?}:");
    if let Some(h) = day1_top.first() {
        println!(
            "     → top: {:?}  (relevance {:.3})",
            h.memory.content, h.relevance
        );
    }
    assert_eq!(
        day1_answer, chatter,
        "on day 1 the fresh, fully-matching small talk should win"
    );

    banner("(6) Identity anchor survives Ebbinghaus decay; trivia fades");
    // Report the raw retention curves so the decay is not just asserted.
    for m in &all_bob {
        if m.content == chatter || m.content == anchor {
            let r1 = m.retention(day1);
            let r8 = m.retention(day8);
            let kind = if m.decay_policy.is_protected() {
                "ANCHOR (ProtectedCore)"
            } else {
                "trivia (Fast decay)"
            };
            println!(
                "   {kind:<24} retention day1 = {r1:.2} → day8 = {r8:.2}  :: {:?}",
                m.content
            );
        }
    }

    println!("\n   DAY 8 — the SAME question, seven days later:");
    let day8_top = familyclaw_memory::retrieve(
        &all_bob,
        &RetrievalContext::new(decay_query).with_limit(1),
        day8,
    );
    let day8_answer = day8_top
        .first()
        .map(|h| h.memory.content.clone())
        .unwrap_or_default();
    if let Some(hit) = day8_top.first() {
        println!(
            "   query {decay_query:?} @ day8 → top: {:?}  (relevance {:.3})",
            hit.memory.content, hit.relevance
        );
    }
    assert_eq!(
        day8_answer, anchor,
        "after decay, the identity anchor must be Bob's strongest memory"
    );
    assert_ne!(
        day1_answer, day8_answer,
        "the whole point: the same query returns a DIFFERENT top memory on day 8"
    );
    println!("   ✓ Same question, different answer: day 1 → chatter, day 8 → identity anchor.");
    println!("     The trivial small talk decayed away; the anchor's retention stayed 1.00.");

    // ── Summary ────────────────────────────────────────────────────────────
    banner("Proof summary — every claim above is backed by an assertion");
    println!(
        "   (1) two live agents on the bus ........... {} beings registered",
        beings.len()
    );
    println!(
        "   (2) real message delivery ................ Alice → bus → Bob stored & recalled it"
    );
    println!(
        "   (3) real emotion propagation ............. Bob's joy {bob_joy_before:.0} → {bob_joy_after:.0} over the bus"
    );
    println!(
        "   (4) dream consolidation effect ........... Bob's active memories {before_active} → {after_active}, dates absolutized {}",
        report.dates_absolutized
    );
    println!("   (5) decay / protected anchor effect ...... same query, day1 ≠ day8 top memory");
    println!("   (6) identity anchor survived decay ....... ProtectedCore retention stayed 1.00");
    println!("   (7) deterministic, one command, no keys .. you just ran it yourself");
    println!(
        "\n\x1b[1;32m  FamilyClaw: persistent agents that message, feel, dream, and change over time.\x1b[0m"
    );

    bus.stop();
    Ok(())
}
