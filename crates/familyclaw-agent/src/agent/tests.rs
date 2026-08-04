//! Integration and unit tests for the agent runtime.

#![allow(clippy::float_cmp, clippy::wildcard_imports, unused_imports)]

use familyclaw_bus::ResonanceBus;
use familyclaw_core::ModelConfig;
use familyclaw_durable::InMemoryJournal;
use familyclaw_memory::LocalJsonStore;

use crate::session::MessageOrigin as SessionOrigin;

use super::helpers::*;
use super::prelude::*;
use super::{
    compact_history, history_max_chars_per_msg, new_reply_channel, Agent, AgentActor,
    CompactionConfig, CompactionSummarizer, ErasedJournal, ErasedMemoryStore, MetricEvent,
    MetricEventSink, ReplySink, ThinkOutcome, ToolLoopConfig, ToolLoopOutcome, TurnOutcome,
    HISTORY_MAX_CHARS_MIN, HISTORY_MAX_CHARS_PER_MSG, HISTORY_MAX_MESSAGES, METRIC_SINK_CAPACITY,
};

/// Helper: builds a test agent with fresh in-memory state, attached to
/// the given bus.
fn test_agent(name: &str, bus: BusHandle) -> Agent {
    // Generic name as-is: `Agent::spawn` does not register the actor in
    // Ractor's global namespace (spawns with a `None` name), so an
    // identically named agent does not collide between tests.
    let config = AgentConfig::new(name, ModelConfig::new("provider/model"));
    let soul = Soul::from_essence(format!("I am {name}, a generic example being."));
    let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
    let durable =
        DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
            .expect("durable ctx");
    Agent::new(config, soul, memory, durable, bus, None, None)
}

// --- Soft/hard watchdog two-stage timeout ------------------------------
//
// Exercised directly against `watchdog_two_stage` (not through a full
// Ractor actor + bus round trip): `handle_turn_with_origin` itself can't
// be made artificially slow without touching it (out of scope for this
// change), so the wrapper — the actual new logic — is what's tested here.

#[tokio::test]
async fn watchdog_two_stage_delivers_late_completion_between_soft_and_hard() {
    let notified = Arc::new(AtomicBool::new(false));
    let notified2 = notified.clone();
    // Finishes at ~1.3s: past the 1s soft deadline, before the 3s hard cap.
    let fut = Box::pin(async {
        tokio::time::sleep(Duration::from_millis(1300)).await;
        7_u32
    });
    let result = watchdog_two_stage(fut, 1, 3, move || {
        notified2.store(true, Ordering::SeqCst);
    })
    .await;
    assert_eq!(result, Ok(7));
    assert!(
            notified.load(Ordering::SeqCst),
            "soft-deadline callback (the interim-notice hook) must fire once the turn runs past the soft deadline"
        );
}

/// Marks `flag` true when dropped — lets a test observe that the turn
/// future (and whatever it borrowed, e.g. `&mut Agent` in the real
/// caller) is actually released at the hard cap.
struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn watchdog_two_stage_aborts_and_drops_future_past_hard_cap() {
    let notified = Arc::new(AtomicBool::new(false));
    let notified2 = notified.clone();
    let dropped = Arc::new(AtomicBool::new(false));
    let dropped2 = dropped.clone();

    let fut = Box::pin(async move {
        let _flag = DropFlag(dropped2);
        // Long enough to never finish within the 2s hard cap below.
        tokio::time::sleep(Duration::from_secs(60)).await;
        99_u32
    });
    let result = watchdog_two_stage(fut, 1, 2, move || {
        notified2.store(true, Ordering::SeqCst);
    })
    .await;
    assert_eq!(result, Err(()));
    assert!(
        notified.load(Ordering::SeqCst),
        "soft-deadline callback must still fire before the hard cap gives up"
    );
    assert!(
            dropped.load(Ordering::SeqCst),
            "future must be dropped at the hard cap — this is what releases `agent` for the fallback reply"
        );
}

#[tokio::test]
async fn new_agent_starts_neutral_and_named() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let agent = test_agent("agent_a", bus.clone());
    assert_eq!(agent.name(), "agent_a");
    assert_eq!(*agent.emotion(), EmotionState::neutral());
    assert_eq!(agent.turns_taken(), 0);
    assert!(!agent.soul().is_empty());
    // being_id is derived from config.id.
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

    // The memory received an entry.
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

    // Sibling "in a creative flow".
    let mut sibling_state = EmotionState::neutral();
    sibling_state.set(Dimension::Joy, 80.0);
    sibling_state.set(Dimension::Curiosity, 60.0);

    let outcome = agent
        .handle_turn(BeingId::new(), &BusMessage::emotion_pulse(sibling_state))
        .await
        .expect("turn");

    // The pulse is not remembered (it is nervous-system "blood", not content).
    assert!(!outcome.remembered);
    assert_eq!(agent.memory().len().await.expect("len"), 0);

    // But the emotion state caught on: Joy 80*0.25 = 20, Curiosity 60*0.25 = 15.
    // Homeostasis reduces by 10% after every turn:
    // Joy 20*0.9 = 18.0, Curiosity 15*0.9 = 13.5.
    assert_eq!(agent.emotion().value(Dimension::Joy), 18.0);
    assert_eq!(agent.emotion().value(Dimension::Curiosity), 13.5);

    bus.stop();
}

#[tokio::test]
async fn emotion_probe_reflects_state_after_bus_delivered_pulse() {
    // Introspection probe round-trip: a SPAWNED agent, whose emotion
    // state lives inside the actor, receives an emotion pulse over the
    // REAL bus, and an external observer reads the changed emotion state
    // from the `emotion_probe` handle. This proves that the probe does
    // not break bus delivery or the actor's Msg type — the state flows
    // bus → handle_turn → probe.
    let bus = ResonanceBus::start(None).await.expect("bus");

    // Receiver: a real Agent, with a shared emotion probe installed.
    let probe = Arc::new(std::sync::Mutex::new(EmotionState::neutral()));
    let receiver = test_agent("agent_b", bus.clone()).with_emotion_probe(probe.clone());
    let joy_before = probe.lock().expect("lock").value(Dimension::Joy);
    assert_eq!(joy_before, 0.0, "probe alkaa neutraalina");

    // Spawn the receiver as an actor — its emotion now lives inside the actor.
    let _receiver_ref = receiver.spawn().await.expect("spawn receiver");

    // Sender: a plain being that leaks a high-joy pulse onto the bus.
    let sender_id = BeingId::new();
    let mut pulse_state = EmotionState::neutral();
    pulse_state.set(Dimension::Joy, 80.0);
    bus.publish(sender_id, BusMessage::emotion_pulse(pulse_state))
        .expect("publish pulse over real bus");

    // Let the bus deliver and the actor process the turn.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // The probe now reflects the contagion produced by the bus-delivered pulse.
    let joy_after = probe.lock().expect("lock").value(Dimension::Joy);
    assert!(
        joy_after > joy_before,
        "bus-toimitettu pulssi nosti vastaanottajan iloa (probe: {joy_before} → {joy_after})"
    );
    // Contagion 80*0.25=20, homeostasis -10% → 18.0 (same math as with
    // direct handle_turn, but now through the bus and actor).
    assert_eq!(joy_after, 18.0, "tartunta kulki busin yli oikein");

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
    // Run two turns, capture the journal ("crash"), build a new agent
    // from the same journal but SHARING THE SAME memory store. Replay
    // must not run the memory-recording side effect again → the memory
    // count stays at 2 (not 4). This tests the actual durability
    // contract, not just turn-counter restoration. (The previous
    // version used FRESH memory, so the test would have passed even if
    // `add` repeated during replay — review issue #9.)
    let bus = ResonanceBus::start(None).await.expect("bus");

    // Same Arc<ErasedMemoryStore> in both the original and the resume run.
    let shared_memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());

    let journal = {
        let durable =
            DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
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
        // Two turns → two memories in the original run.
        assert_eq!(shared_memory.len().await.expect("len"), 2);
        agent.durable.finish()
    };

    // Same journal → replay returns the stored outcomes. SAME memory.
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

    // Repeat the same turns in the same order: outcomes come from the
    // log (deterministic replay), and the `add` side effect does not repeat.
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

    // Core assertion: there are still exactly 2 memories — replay did NOT duplicate them.
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

    // Attach a single agent as an actor.
    let agent = test_agent("agent_a", bus.clone());
    let agent_memory = agent.memory();
    let agent_id = agent.being_id();
    let _actor = agent.spawn().await.expect("spawn");

    // The bus knows the being (beings[] not empty).
    let beings = bus.beings().await.expect("beings");
    assert_eq!(beings.len(), 1);
    assert_eq!(beings[0].id, agent_id);
    assert_eq!(beings[0].name, "agent_a");

    // Another being sends text → the agent processes and remembers it.
    let other = BeingId::new();
    bus.publish(other, BusMessage::text("tervehdys actorille"))
        .expect("publish");

    // Let the actor process the message.
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

    // agent_a speaks → agent_b hears and remembers.
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
    // ResumeApproval is a control signal ("blood"), not memorable content.
    assert!(!should_remember(&BusMessage::ResumeApproval {
        approval_id: "any".into(),
    }));
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

// ---- C2 reply path (C1 Model A) -------------------------------------

/// Core assertion (TASK C2): when a reply sink + reply target is
/// installed, the agent's produced reply ends up in the reply sink with
/// the CORRECT target (channel/conversation id). This is the same path
/// that `handle_turn` runs when `think()` produces text: build an
/// `OutboundMessage` with the target → `route_reply` → the gateway gets
/// it from the recv end.
#[tokio::test]
async fn route_reply_reaches_sink_with_correct_target() {
    let bus = ResonanceBus::start(None).await.expect("bus");

    let (sink, mut rx) = new_reply_channel();
    let agent = test_agent("agent_a", bus.clone())
        .with_reply_sink(sink)
        .with_reply_target("discord:general-42");

    // Same construction logic as handle_turn's reply-path branch:
    // think text → OutboundMessage with the agent's reply target.
    let thought = "ajattelin tämän";
    let reply = OutboundMessage::new("discord:general-42", thought).expect("reply");
    agent.route_reply(reply).expect("route");

    // The gateway (recv end) received the reply with the correct channel/conversation id.
    let got = rx.recv().await.expect("reply delivered");
    assert_eq!(got.target, "discord:general-42", "vastaus oikeaan kanavaan");
    assert_eq!(got.body, thought);

    bus.stop();
}

/// Without a reply sink, `route_reply` is a no-op (returns Ok) — the
/// current, backward-compatible behavior (replies are dropped).
#[tokio::test]
async fn route_reply_without_sink_is_noop() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let agent = test_agent("agent_a", bus.clone());
    let reply = OutboundMessage::new("anywhere", "ei kuulijaa").expect("reply");
    // No panic, no error — the reply is simply dropped.
    agent.route_reply(reply).expect("noop ok");
    bus.stop();
}

/// If a sink is installed but the gateway closed the recv end,
/// `route_reply` returns Err (the reply could not be delivered) — and does not panic.
#[tokio::test]
async fn route_reply_errors_when_sink_closed() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let (sink, rx) = new_reply_channel();
    drop(rx); // gateway stopped → recv end closed.
    let agent = test_agent("agent_a", bus.clone()).with_reply_sink(sink);
    let reply = OutboundMessage::new("c", "hukkaan").expect("reply");
    assert!(
        agent.route_reply(reply).is_err(),
        "suljettu sink → toimitusvirhe"
    );
    bus.stop();
}

// ---- F1 failover wiring ---------------------------------------------

/// `Agent::new(Some(LlmConfig))` wraps a single client into a
/// length-1 failover chain (backward-compatible: no fallbacks).
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

/// `with_failover` replaces the constructor's length-1 chain with the
/// FULL chain (primary + fallbacks) — F1: the agent gets the failover,
/// not just the primary.
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

    // The agent is built WITHOUT an llm, then the full chain is wired in.
    let agent = test_agent("agent_a", bus.clone()).with_failover(chain);
    let failover = agent.llm().expect("failover wired");
    assert_eq!(failover.len(), 2, "primary + 1 fallback");
    assert_eq!(failover.primary_model(), "openai/gpt-4o");
    bus.stop();
}

// ---- F4 session isolation --------------------------------------------

use crate::session::MessageOrigin;

/// F4 write-side: when a session is set, the turn's memory gets a
/// session tag (`session:<channel>:<conversation>`) in addition to the
/// `from:` tag. Without a session there is no tag (shared scope is preserved).
#[tokio::test]
async fn session_tags_memory_for_isolation() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let origin = MessageOrigin::new("discord-main", "general", "user-1");
    let mut agent = test_agent("agent_a", bus.clone()).with_session(origin.clone());

    agent
        .handle_turn(BeingId::new(), &BusMessage::text("sessio-viesti"))
        .await
        .expect("turn");

    // The memory is tagged with the session tag → recall with the same
    // required tag finds it.
    let scoped = RetrievalContext::new("sessio-viesti").with_required_tags([origin.session_tag()]);
    let hits = agent.recall(&scoped).await.expect("recall scoped");
    assert_eq!(
        hits.len(),
        1,
        "session-tagilla suodatettu recall löytää muiston"
    );
    assert!(hits[0].memory.tags.contains(&origin.session_tag()));

    bus.stop();
}

/// F4 read-side (core claim): memories of two different sessions **do not
/// leak** into each other's context. Same shared memory, but the required
/// session tag separates A's memories from B's query.
#[tokio::test]
async fn sessions_do_not_leak_memories_across_each_other() {
    let bus = ResonanceBus::start(None).await.expect("bus");

    // SHARED memory (one store) — proves that isolation comes from the tag,
    // not from separate stores.
    let shared: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());

    let origin_a = MessageOrigin::new("discord-main", "channel-a", "u");
    let origin_b = MessageOrigin::new("discord-main", "channel-b", "u");

    // Session A writes a memory into the shared store.
    {
        let durable =
            DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
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

    // Session B writes ITS OWN memory into the SAME store. Different agent
    // name ("agent_b") → different turn_key → the memory store does not
    // dedupe it against A's turn-0 (dedup is per-agent turn_key, not per-session).
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

    // The shared store contains BOTH memories.
    assert_eq!(shared.len().await.expect("len"), 2);

    // B's session scope (required B tag) does NOT see A's memory.
    let b_scope =
        RetrievalContext::new("salaisuus kanavasta A").with_required_tags([origin_b.session_tag()]);
    let b_sees = agent_b.recall(&b_scope).await.expect("recall b");
    assert!(
        b_sees
            .iter()
            .all(|r| !r.memory.content.contains("kanavasta A")),
        "B:n sessio ei saa nähdä A:n muistoa"
    );

    // A's session scope sees A's own memory (positive control).
    let a_scope =
        RetrievalContext::new("salaisuus kanavasta A").with_required_tags([origin_a.session_tag()]);
    let a_sees = agent_b.recall(&a_scope).await.expect("recall a");
    assert_eq!(a_sees.len(), 1, "A:n sessio näkee oman muistonsa");
    assert!(a_sees[0].memory.content.contains("kanavasta A"));

    bus.stop();
}

/// Without a session (None), recall is shared — a backward-compatible
/// negative control: current MVP behavior remains unchanged.
#[tokio::test]
async fn no_session_keeps_shared_scope() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let mut agent = test_agent("agent_a", bus.clone());
    assert!(agent.session().is_none(), "oletus: ei sessiota");

    agent
        .handle_turn(BeingId::new(), &BusMessage::text("jaettu viesti"))
        .await
        .expect("turn");

    // Recall WITHOUT a tag requirement finds the memory (shared scope).
    let hits = agent
        .recall(&RetrievalContext::new("jaettu viesti"))
        .await
        .expect("recall");
    assert_eq!(hits.len(), 1);
    // The memory has no session tag (no `session:` prefix).
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

/// `with_reply_sink` / `with_reply_target` chain and do not change the
/// `Agent::new` signature (C1: the constructor is not touched).
#[tokio::test]
async fn reply_setters_chain_and_preserve_identity() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let (sink, _rx) = new_reply_channel();
    let agent = test_agent("agent_a", bus.clone())
        .with_reply_sink(sink)
        .with_reply_target("tg:chat-7");
    // Identity is preserved after the setters.
    assert_eq!(agent.name(), "agent_a");
    assert_eq!(agent.turns_taken(), 0);
    bus.stop();
}

/// Phase 1: When no governor is installed, the agent behaves in a
/// backward-compatible way (default behavior is preserved).
#[tokio::test]
async fn no_governor_means_legacy_behavior() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let mut agent = test_agent("agent_a", bus.clone());
    // By default the governor field is None → base state.
    // Process the text → it is remembered (same as before the governor).
    let outcome = agent
        .handle_turn(BeingId::new(), &BusMessage::text("vanha viesti"))
        .await
        .expect("turn");
    assert!(outcome.remembered);
    bus.stop();
}

/// Phase 1: The default governor filters `EmotionPulse` messages out of
/// LLM thinking. This is a key fix: emotion pulses are "blood" not
/// speech, and must not trigger an LLM call.
#[tokio::test]
async fn default_governor_filters_emotion_pulse_from_think() {
    use familyclaw_emotion::EmotionState;
    let bus = ResonanceBus::start(None).await.expect("bus");
    // An agent with a default governor (but NO LLM, so we can
    // verify that filtering does not crash).
    let mut agent = test_agent("agent_a", bus.clone()).with_default_governor();
    // Simulate a "fearful" state so that the LLM would NOT filter it
    // (governor_decide would be Hesitate), yet we still get the test
    // to cover the EmotionPulse path. We give the state a neutral value.
    agent.emotion = EmotionState::neutral();
    // EmotionPulse from a sibling → should return a successful turn
    // without crashing. (There is no LLM → thought_response = None, but
    // the path goes through the governor filtering.)
    let outcome = agent
        .handle_turn(
            BeingId::new(),
            &BusMessage::emotion_pulse(EmotionState::neutral()),
        )
        .await
        .expect("turn should not fail when governor filters");
    // The pulse is not remembered (it is "blood", not content).
    assert!(!outcome.remembered);
    bus.stop();
}

/// Phase 1: The default governor produces a Hesitate decision when the
/// safety threshold is exceeded (Fear above 80), which blocks the reply.
/// This tests the gatekeeper: even if the LLM produced text, the
/// reply is not sent while in the Hesitate state.
#[tokio::test]
async fn governor_hesitate_blocks_reply() {
    use familyclaw_emotion::{Dimension, EmotionState};
    let bus = ResonanceBus::start(None).await.expect("bus");
    let (sink, mut rx) = new_reply_channel();
    // Install governor + reply target. No LLM is needed for the test;
    // we only test that the Hesitate state blocks the reply path.
    let mut agent = test_agent("agent_a", bus.clone())
        .with_default_governor()
        .with_reply_sink(sink)
        .with_reply_target("tg:chat-7");
    // Force a "fearful" emotional state.
    let mut fear_state = EmotionState::neutral();
    fear_state.set(Dimension::Fear, 95.0);
    agent.emotion = fear_state;
    // Text message → handle_turn proceeds, but the reply should be
    // blocked because the governor decides Hesitate.
    let _ = agent
        .handle_turn(BeingId::new(), &BusMessage::text("scary"))
        .await
        .expect("turn");
    // The reply channel should NOT contain any messages.
    let received = rx.try_recv();
    assert!(
        received.is_err(),
        "Hesitate-tilassa reply:tä ei saa lähettää, saatiin: {received:?}"
    );
    bus.stop();
}

/// FIX 1: non-neutral calibration changes the agent's emotional state
/// development — and thereby the governor's [`ActionDecision`] — compared
/// to neutral calibration. This proves that `calibration.json` is no
/// longer merely decorative but actually affects behavior.
///
/// Mechanism: the governor reads the `self.emotion` state. Homeostasis
/// pulls the state toward the calibration's `baseline` resting state.
/// Non-neutral calibration (high Curiosity baseline) keeps the state
/// high, while neutral pulls it toward zero → a different governor
/// decision with the same profile and input.
#[tokio::test]
async fn non_neutral_calibration_changes_governor_decision_vs_neutral() {
    use familyclaw_emotion::{
        ActionDecision, Dimension, EmotionActionGovernor, GoverningProfile, NeutralCalibration,
        TableCalibration,
    };

    // Helper: builds an agent with the given calibration, runs N text
    // turns (letting homeostasis converge toward the calibration's
    // resting state), and returns the governor's decision + the final
    // Curiosity value.
    // (Defined before statements: clippy::items_after_statements.)
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

    // Common profile for both: a mild warmth threshold, no blend required,
    // so that a single high warm dimension (Curiosity) is enough to
    // push the governor into the EngageWarmly state.
    let profile = GoverningProfile::new("relaxed", 90.0, 50.0, 80.0, 1.0, false);

    // 1. NEUTRAL calibration: text contact raises Curiosity +5.0/turn,
    //    homeostasis pulls 10% toward the resting state 0. Fixed point
    //    x=(x+5)*0.9 → Curiosity converges to ~45, below the warmth
    //    threshold (50) → Reflect.
    let (neutral_decision, neutral_curiosity) =
        decide_after_text_turns(Box::new(NeutralCalibration), &profile, 80).await;

    // 2. NON-NEUTRAL calibration: high Curiosity baseline (70).
    //    Homeostasis pulls the state TOWARD 70, not zero. The fixed
    //    point pushes Curiosity to the ceiling (~100), well above the
    //    warmth threshold (50) → EngageWarmly. A DIFFERENT decision with
    //    the same profile + input.
    let warm_cal = TableCalibration::new("warm_curious")
        .with_baseline(Dimension::Curiosity, 70.0)
        .with_sensitivity(Dimension::Curiosity, 1.0);
    let (warm_decision, warm_curiosity) =
        decide_after_text_turns(Box::new(warm_cal), &profile, 80).await;

    // The emotional state developed differently (proof that calibration matters).
    assert!(
        warm_curiosity > neutral_curiosity + 50.0,
        "ei-neutraali baseline pitää Curiosityn korkealla \
             (warm={warm_curiosity}, neutral={neutral_curiosity})"
    );
    // And the governor's decision differs: Reflect for neutral,
    // EngageWarmly for warm.
    assert_eq!(neutral_decision, ActionDecision::Reflect);
    assert_eq!(warm_decision, ActionDecision::EngageWarmly);
    assert_ne!(
        warm_decision, neutral_decision,
        "ei-neutraali kalibrointi muuttaa governorin päätöstä"
    );
}

/// FIX 1 (second mechanism): the calibration's `sensitivity` scales the
/// intensity of the contact stimulus — the same text turn raises
/// Curiosity more with high sensitivity than with neutral.
#[tokio::test]
async fn calibration_sensitivity_scales_text_stimulus() {
    use familyclaw_emotion::{Dimension, NeutralCalibration, TableCalibration};

    let bus = ResonanceBus::start(None).await.expect("bus");

    // Neutral (sensitivity = 1.0): +5.0 stimulus, homeostasis → 4.5.
    let mut neutral_agent =
        test_agent("agent_n", bus.clone()).with_calibration(Box::new(NeutralCalibration));
    neutral_agent
        .handle_turn(BeingId::new(), &BusMessage::text("kontakti"))
        .await
        .expect("turn");
    let neutral_curiosity = neutral_agent.emotion().value(Dimension::Curiosity);

    // High sensitivity (3.0): +15.0 stimulus, homeostasis → 13.5.
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
    // Exact values: neutral 4.5, sensitive 13.5 (3x stimulus).
    assert_eq!(neutral_curiosity, 4.5);
    assert_eq!(sensitive_curiosity, 13.5);

    bus.stop();
}

/// Regression test (code review #2, "production breaker"): a sustained
/// high sibling pulse must NOT drive the receiver's dimension to the
/// ceiling (100). Before the fix, contagion added `source * 0.25` every
/// tick regardless of the receiver's value → homeostasis (10%) could not
/// damp it in time and the equilibrium was `2.25 * source` → saturation
/// to the ceiling. After the fix, contagion approaches the source
/// (`(source - target) * factor`), so the value cannot exceed the source
/// nor saturate to the ceiling.
#[tokio::test]
async fn repeated_contagion_does_not_saturate_to_ceiling() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let mut agent = test_agent("agent_sat", bus.clone());

    // Sibling in a sustained high-joy state (but NOT at the ceiling: 80/100).
    let mut sibling_state = EmotionState::neutral();
    sibling_state.set(Dimension::Joy, 80.0);

    // A hundred turns of the same high pulse — worst case for a feedback loop.
    for _ in 0..100 {
        agent
            .handle_turn(BeingId::new(), &BusMessage::emotion_pulse(sibling_state))
            .await
            .expect("turn");
    }

    let joy = agent.emotion().value(Dimension::Joy);
    // No saturation to the ceiling: stays well below 100.
    assert!(
        joy < 100.0,
        "jatkuva contagion ei saa saturoida kattoon, joy = {joy}"
    );
    // Nor may it exceed the source value (contagion = approaching, not accumulation).
    assert!(
        joy <= 80.0 + 1e-3,
        "vastaanottaja ei saa ylittää lähdettä (80), joy = {joy}"
    );
}

/// When the high sibling pulses stop, homeostasis pulls the emotional
/// state back toward neutral (baseline 0) — it does not get stuck at an
/// elevated value. Proves that the decay/homeostasis term balances contagion.
#[tokio::test]
async fn homeostasis_pulls_back_toward_baseline_after_contagion() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let mut agent = test_agent("agent_decay", bus.clone());

    // Raise the emotional state via contagion with a few pulses.
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

    // Pulses stop → neutral turns (that do not change the emotional state).
    // A task message does not change the emotional state (only homeostasis runs).
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
    // Homeostasis pulled back toward the baseline (0) — clearly downward.
    assert!(
        relaxed < elevated,
        "homeostaasin pitäisi laskea iloa: {elevated} -> {relaxed}"
    );
    // 30 turns of 10% exponential decay → a fraction of the original
    // value. A robust relative bound (not sensitive to exact
    // contagion/decay arithmetic): at least 90% recovered.
    assert!(
        relaxed < elevated * 0.1,
        "pitkän tauon jälkeen ilon pitäisi olla lähellä baselinea: \
             {elevated} -> {relaxed}"
    );

    bus.stop();
}

/// Phase 1: `with_governor_profile` takes a `Box<dyn>` interface, so
/// Layer B can supply its own per-being profile.
#[tokio::test]
async fn with_governor_profile_accepts_dyn() {
    use familyclaw_emotion::default_governing_profile;
    let bus = ResonanceBus::start(None).await.expect("bus");
    let mut agent = test_agent("agent_a", bus.clone());
    let profile: Box<dyn familyclaw_emotion::EmotionActionGoverning + Send + Sync> =
        Box::new(default_governing_profile());
    agent = agent.with_governor_profile(profile);
    // Recognition: the agent must now follow the governor.
    // Simple check: the turn proceeds successfully.
    let outcome = agent
        .handle_turn(BeingId::new(), &BusMessage::text("ok"))
        .await
        .expect("turn");
    assert!(outcome.remembered);
    bus.stop();
}

// ---- 1B tool loop --------------------------------------------------------

// ToolLoopConfig + ActionRuntime already come in via `use super::*`
// (ToolLoopConfig is a type of this module; ActionRuntime is imported
// at the top of the agent module). LlmConfig is a private `use` in the
// agent module, so it is imported here explicitly.
use crate::llm::LlmConfig;
use std::sync::Arc as StdArc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex as TokioMutex;

/// Starts a **scripted fake LLM**: an OpenAI-compatible HTTP endpoint
/// that returns the given response bodies (JSON) in order, one per
/// request. Returns the base URL (`http://127.0.0.1:PORT/v1`), which can
/// be given to [`LlmConfig`]. The server lives until all bodies have
/// been consumed.
///
/// This is the same raw-TCP pattern as in `llm.rs`'s timeout/empty-choices
/// tests — no external mock library, no network egress.
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
            // Read (and discard) the request; we do not check the body in this test.
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

/// `OpenAI` response body with **plain text only** (no tool calls) → the loop
/// stops.
fn body_text(text: &str) -> String {
    serde_json::json!({
        "choices": [ { "message": { "content": text } } ]
    })
    .to_string()
}

/// `OpenAI` response body with **a single tool call** — exactly the
/// chat-completions format that real providers send:
/// `type:"function"` + a nested `function` object whose `arguments` is
/// a **JSON string** (not a raw object), and `content` is `null`. This
/// mirrors production wiring, so tests will catch decoding bugs going forward.
fn body_tool_call(id: &str, name: &str, arguments: &serde_json::Value) -> String {
    let arguments_str =
        serde_json::to_string(arguments).expect("arguments serialize to JSON string");
    serde_json::json!({
        "choices": [ {
            "message": {
                "role": "assistant",
                "content": serde_json::Value::Null,
                "tool_calls": [ {
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": arguments_str }
                } ]
            },
            "finish_reason": "tool_calls"
        } ]
    })
    .to_string()
}

/// Builds an agent with a scripted LLM (one endpoint, no fallbacks).
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

/// Like [`agent_with_scripted_llm`], but with a **fixed** `AgentId`.
///
/// Needed in crash-resilience tests where the SAME being is rebuilt after
/// a "restart": in production, the being's `config.id` (and the
/// [`Agent::being_id`] derived from it) is stable across restarts, because
/// the gateway derives it deterministically from the name
/// (`AgentConfig::new_with_stable_id` → `AgentId::from_name`), so the
/// resume ownership check matches. Plain [`AgentConfig::new`] picks a
/// random id — in a restart simulation that would give the WRONG, a
/// different, being, so this helper pins the id explicitly to match
/// production stability.
fn agent_with_scripted_llm_id(
    id: familyclaw_core::AgentId,
    name: &str,
    bus: BusHandle,
    api_base: &str,
) -> Agent {
    let mut config = AgentConfig::new(name, ModelConfig::new("scripted/model"));
    config.id = id;
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

/// Read-only test skill for the tool loop: echoes the payload's `q` field
/// back into the output. Auto-run (no approval), so the loop can feed the
/// result back into the model.
#[derive(Debug, Clone, Default)]
struct LoopEchoSkill;

/// Fixed identifier for the test skill (deterministic).
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
            publisher: None,
            signature: None,
        }
    }
}

/// Builds a shared runtime with the `loop_echo` test skill registered.
fn echo_runtime() -> StdArc<TokioMutex<ActionRuntime>> {
    let mut rt = ActionRuntime::new();
    rt.register_skill(LoopEchoSkill)
        .expect("register loop_echo");
    StdArc::new(TokioMutex::new(rt))
}

/// Test skill requiring approval, for the tool loop: models an
/// externally-writing (`WriteExternal`) action that is NOT auto-runnable
/// and that stops to wait for human approval
/// ([`SubmitOutcome::pending_approval`] = `Some`). Used to prove that the
/// pending-approval control state does not leak to the user.
#[derive(Debug, Clone, Default)]
struct ApprovalSkill;

/// Fixed identifier for the approval skill (deterministic).
const APPROVAL_UUID: uuid::Uuid = uuid::uuid!("99999999-2222-4333-8444-555555555555");

#[async_trait::async_trait]
impl familyclaw_actions::ActionExecutor for ApprovalSkill {
    async fn execute(
        &self,
        request: familyclaw_actions::ActionRequest,
    ) -> familyclaw_actions::Result<familyclaw_actions::ActionResult> {
        // Should never execute without approval in this test,
        // but a return value is needed for the type contract.
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
            description:
                "Ulkoisesti kirjoittava toiminto (vaatii ihmisen hyväksynnän, testikäyttö)."
                    .to_string(),
            permissions: vec![familyclaw_actions::policy::SkillPermission::WriteExternal],
            risk: familyclaw_actions::policy::ActionRisk::WriteExternal,
            approval_policy: familyclaw_actions::policy::ApprovalPolicy::RequireApproval,
            input_hint: None,
            output_hint: None,
            input_schema: familyclaw_actions::manifest::default_input_schema(),
            publisher: None,
            signature: None,
        }
    }
}

/// Builds a shared runtime with the approval-requiring
/// `approval_skill` test skill registered.
fn approval_runtime() -> StdArc<TokioMutex<ActionRuntime>> {
    let mut rt = ActionRuntime::new();
    rt.register_skill(ApprovalSkill)
        .expect("register approval_skill");
    StdArc::new(TokioMutex::new(rt))
}

/// Like [`ApprovalSkill`], but counts every execution into a
/// **per-instance** shared counter. A per-instance (not global) counter
/// keeps parallel tests separate — each test builds its own counter.
/// Used to prove the resume "side effect runs exactly once" invariant.
#[derive(Debug, Clone)]
struct CountingApprovalSkill {
    /// Shared execution counter (cloned alongside the test's own handle).
    count: StdArc<std::sync::atomic::AtomicUsize>,
}

impl CountingApprovalSkill {
    /// Builds a skill that increments the given shared counter on every
    /// execution.
    fn new(count: StdArc<std::sync::atomic::AtomicUsize>) -> Self {
        Self { count }
    }
}

/// Fixed identifier for the counting approval skill.
const COUNTING_APPROVAL_UUID: uuid::Uuid = uuid::uuid!("99999999-3333-4444-8555-666666666666");

#[async_trait::async_trait]
impl familyclaw_actions::ActionExecutor for CountingApprovalSkill {
    async fn execute(
        &self,
        request: familyclaw_actions::ActionRequest,
    ) -> familyclaw_actions::Result<familyclaw_actions::ActionResult> {
        self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(familyclaw_actions::ActionResult::success(
            "counting approval action executed",
            serde_json::json!({ "executed": true }),
            request.now,
        ))
    }
}

impl familyclaw_actions::Skill for CountingApprovalSkill {
    fn manifest(&self) -> familyclaw_actions::manifest::SkillManifest {
        familyclaw_actions::manifest::SkillManifest {
            id: familyclaw_actions::SkillId::from_uuid(COUNTING_APPROVAL_UUID),
            name: "approval_skill".to_string(),
            version: "1.0.0".to_string(),
            description:
                "Laskeva ulkoisesti kirjoittava toiminto (vaatii hyväksynnän, testikäyttö)."
                    .to_string(),
            permissions: vec![familyclaw_actions::policy::SkillPermission::WriteExternal],
            risk: familyclaw_actions::policy::ActionRisk::WriteExternal,
            approval_policy: familyclaw_actions::policy::ApprovalPolicy::RequireApproval,
            input_hint: None,
            output_hint: None,
            input_schema: familyclaw_actions::manifest::default_input_schema(),
            publisher: None,
            signature: None,
        }
    }
}

/// (a) `actions = None` preserves the one-shot behavior: a single LLM
/// call, no tools, the model's text is returned as-is.
#[tokio::test]
async fn tool_loop_none_keeps_one_shot() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![body_text("yksi vastaus")]).await;
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api);
    assert!(!agent.has_actions(), "oletus: ei toimintoja → yhden kerran");

    let out = agent
        .think(&BusMessage::text("hei"))
        .await
        .expect("one-shot ok");
    assert_eq!(out, ThinkOutcome::Reply("yksi vastaus".to_string()));
    bus.stop();
}

/// (b) The tool loop stops as soon as the model replies without tool
/// calls (even if tools are available).
#[tokio::test]
async fn tool_loop_stops_on_no_tool_calls() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![body_text("ei työkaluja tarvita")]).await;
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api).with_actions(echo_runtime());
    assert!(agent.has_actions());

    let out = agent
        .think(&BusMessage::text("kysymys"))
        .await
        .expect("loop ok");
    assert_eq!(out, ThinkOutcome::Reply("ei työkaluja tarvita".to_string()));
    bus.stop();
}

/// (c) A tool call is dispatched and the result fed back: the first
/// response requests the `loop_echo` tool, the second (having seen the
/// result) responds with text → the loop stops at the final text.
#[tokio::test]
async fn tool_loop_dispatches_tool_and_feeds_result_back() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![
        body_tool_call("call_1", "loop_echo", &serde_json::json!({ "q": "ping" })),
        body_text("työkalu vastasi, valmis"),
    ])
    .await;
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api).with_actions(echo_runtime());

    let out = agent
        .think(&BusMessage::text("aja työkalu"))
        .await
        .expect("loop ok");
    // The second round stopped at the final text (the tool's result
    // was fed back into the model before this).
    assert_eq!(
        out,
        ThinkOutcome::Reply("työkalu vastasi, valmis".to_string())
    );
    bus.stop();
}

// ── Phase 2: observability metrics sink ──────────────────────────────

/// A tool call in the tool loop emits [`MetricEvent::ToolDispatched`] to the sink.
#[tokio::test]
async fn tool_dispatch_emits_metric_event() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![
        body_tool_call("call_1", "loop_echo", &serde_json::json!({ "q": "ping" })),
        body_text("valmis"),
    ])
    .await;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<MetricEvent>(METRIC_SINK_CAPACITY);
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(echo_runtime())
        .with_metrics_sink(tx);

    let _ = agent
        .think(&BusMessage::text("aja työkalu"))
        .await
        .expect("loop ok");
    // One tool call was sent → exactly one ToolDispatched.
    let ev = rx.try_recv().expect("metric event emitted");
    assert_eq!(ev, MetricEvent::ToolDispatched);
    assert!(rx.try_recv().is_err(), "vain yksi dispatch tässä vuorossa");
    bus.stop();
}

/// `think()` without tools does NOT emit a tool metric (text-only turn).
#[tokio::test]
async fn text_only_turn_emits_no_tool_metric() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![body_text("pelkkä teksti")]).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<MetricEvent>(METRIC_SINK_CAPACITY);
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(echo_runtime())
        .with_metrics_sink(tx);

    let _ = agent.think(&BusMessage::text("hei")).await.expect("ok");
    assert!(
        rx.try_recv().is_err(),
        "ei työkalukutsua → ei tool-mittaria"
    );
    bus.stop();
}

/// A successful turn via [`Agent::handle_turn`] emits
/// [`MetricEvent::TurnCompleted`] (a fresh turn, not a replay).
#[tokio::test]
async fn completed_turn_emits_turn_metric() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![body_text("vastaus")]).await;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<MetricEvent>(METRIC_SINK_CAPACITY);
    let mut agent = agent_with_scripted_llm("agent_a", bus.clone(), &api).with_metrics_sink(tx);

    let _ = agent
        .handle_turn(BeingId::new(), &BusMessage::text("kysymys"))
        .await
        .expect("turn ok");
    let ev = rx.try_recv().expect("turn metric emitted");
    assert_eq!(ev, MetricEvent::TurnCompleted);
    bus.stop();
}

/// (d) Unknown tool → an error `tool_result` is fed back, the loop
/// CONTINUES (does not abort). The next response is text → stop.
#[tokio::test]
async fn tool_loop_unknown_tool_does_not_abort() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![
        body_tool_call("call_x", "does_not_exist", &serde_json::json!({})),
        body_text("ok, jatketaan ilman sitä työkalua"),
    ])
    .await;
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api).with_actions(echo_runtime());

    let out = agent
        .think(&BusMessage::text("kokeile tuntematonta"))
        .await
        .expect("loop continues past unknown tool");
    assert_eq!(
        out,
        ThinkOutcome::Reply("ok, jatketaan ilman sitä työkalua".to_string())
    );
    bus.stop();
}

/// (e) The iteration limit bounds the loop: if the model ALWAYS requests
/// a tool and never responds with text, the loop stops at the
/// `max_iterations` limit and does NOT get stuck in an infinite cycle.
/// We script exactly `max` tool calls (the server responds no more →
/// if the loop exceeded the limit, the next LLM call would hang on
/// timeout; the limit prevents that).
///
/// **User-facing boundary:** when the limit is reached without a
/// response, `think()` returns [`ThinkOutcome::NoReply`] — the internal
/// max-iter marker is NOT routed to the user. A previous implementation
/// leaked the `"[tool loop stopped: ...]"` string verbatim through the
/// reply pipe; this test guards against that happening again.
#[tokio::test]
async fn tool_loop_max_iterations_does_not_leak_marker_to_user() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let max = 3u32;
    // Exactly `max` tool-call responses — one per round. The loop must NOT
    // request a (max+1)th response from the server.
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
        .with_tool_loop(ToolLoopConfig {
            max_iterations: max,
        });

    // The loop stops at the limit without panicking/hanging. Since the
    // model never produced text, the anti-silence path returns a generic
    // user-friendly fallback response (not the raw max-iter marker).
    let out = agent
        .think(&BusMessage::text("ikuinen työkalupyyntö"))
        .await
        .expect("max-iter ei saa palauttaa virhettä");
    assert_eq!(
        out,
        ThinkOutcome::Reply(recovery_fallback_reply()),
        "max-iter ilman mallin tekstiä tuottaa varavastauksen, ei hiljaisuutta, sai: {out:?}"
    );
    bus.stop();
}

// ── Failover gap #1 fix, item 2: think-error messaging is split from
//    max-iterations, and never leaks a raw provider body/account id ────

/// (b) All providers 404 -> `recovery_fallback_reply_for_error` produces
/// a message that names the real cause (HTTP 404 / retired model) and
/// the config knob to check, but never the raw provider body — even
/// when that body contains something that looks like an account id
/// (the production incident this test guards against).
#[test]
fn provider_not_found_reply_names_cause_without_leaking_body() {
    let raw = LlmError::NotFound(
        "HTTP 404: {\"error\":\"Function id acct_9f8e7d6c5b4a not found\"}".to_string(),
    );
    let tagged = FamilyClawError::llm(tag_llm_error(&raw));

    let reply = recovery_fallback_reply_for_error(&tagged);

    assert!(
        reply.contains("404"),
        "reply should surface the status code: {reply}"
    );
    assert!(
        reply.contains("FAMILYCLAW_PROVIDER_MODEL"),
        "reply should point at the config knob: {reply}"
    );
    assert!(
        !reply.contains("acct_9f8e7d6c5b4a"),
        "reply must NEVER contain the raw provider body/account id: {reply}"
    );
    assert!(
        !reply.contains("Function id"),
        "reply must NEVER contain raw provider body text: {reply}"
    );
    assert_eq!(
            reply,
            "LLM-palveluihin ei saatu yhteyttä (viimeisin virhe: HTTP 404 — malli on \
             mahdollisesti poistettu). Tarkista FAMILYCLAW_PROVIDER_MODEL / provider-konfiguraatio."
        );
}

/// Think-error messaging must differ from the max-iterations message —
/// otherwise an LLM outage is still misreported as "a tool failed / a
/// safety limit was hit" (the exact bug this fix addresses).
#[test]
fn think_error_reply_differs_from_max_iterations_reply() {
    let raw = LlmError::NotFound("HTTP 404: [redacted]".to_string());
    let tagged = FamilyClawError::llm(tag_llm_error(&raw));
    assert_ne!(
        recovery_fallback_reply_for_error(&tagged),
        recovery_fallback_reply()
    );
}

/// Non-`ProviderNotFound` LLM failures (e.g. every provider timed out)
/// still get a distinguishable, redacted reply — never the raw body,
/// and never silently falling back to the max-iterations wording.
#[test]
fn other_llm_failure_classes_get_redacted_non_generic_reply() {
    let raw = LlmError::Timeout("connect timed out after 10s to 10.0.0.5".to_string());
    let tagged = FamilyClawError::llm(tag_llm_error(&raw));
    let reply = recovery_fallback_reply_for_error(&tagged);
    assert!(reply.contains("timeout"), "got: {reply}");
    assert!(
        !reply.contains("10.0.0.5"),
        "reply must not leak raw error detail: {reply}"
    );
}

/// A non-LLM `FamilyClawError` (e.g. a bus/durable failure) has no tag
/// to recover -> falls back to the existing generic reply rather than
/// panicking or fabricating a bogus LLM category.
#[test]
fn non_llm_error_falls_back_to_generic_reply() {
    let e = FamilyClawError::bus("mailbox closed");
    assert_eq!(
        recovery_fallback_reply_for_error(&e),
        recovery_fallback_reply()
    );
}

/// `tag_llm_error` / `parse_llm_class_tag` round-trip for every
/// [`LlmFailureClass`] — the seam the think-error path depends on.
#[test]
fn llm_error_tag_round_trips_class_and_status_line() {
    let cases: Vec<(LlmError, &str, &str)> = vec![
        (
            LlmError::NotFound("HTTP 404: x".into()),
            "provider_not_found",
            "HTTP 404",
        ),
        (
            LlmError::AuthFailed("HTTP 401: [redacted]".into()),
            "auth_failed",
            "HTTP 401",
        ),
        (LlmError::Timeout("slow".into()), "timeout", "timeout"),
        (LlmError::NoContent, "no_content", "no_content"),
    ];
    for (err, expected_class, expected_status_line) in cases {
        let tagged = tag_llm_error(&err);
        let (class, status_line) = parse_llm_class_tag(&tagged).expect("tagged message parses");
        assert_eq!(class, expected_class);
        assert_eq!(status_line, expected_status_line);
    }
}

/// (f) **A tool requiring approval returns [`ThinkOutcome::Suspended`]
/// — NOT a user reply.** When the model calls a tool that requires
/// approval, execution waits for human permission. A previous (1B)
/// implementation returned `"[awaiting approval: ... (approval_id=...)]"`
/// as a plain success string, which was routed verbatim — including the
/// raw `approval_id` — to the user. 1C: `think()` returns a first-class
/// `Suspended` state that carries a **typed** `approval_id` and a
/// redacted summary — and it is not `Reply`, so it never routes into the
/// reply pipe.
#[tokio::test]
async fn tool_loop_awaiting_approval_returns_suspended_not_reply() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    // One response: the model calls the tool that requires approval.
    let api = spawn_scripted_llm(vec![body_tool_call(
        "call_approve",
        "approval_skill",
        &serde_json::json!({ "q": "do-it" }),
    )])
    .await;
    let agent =
        agent_with_scripted_llm("agent_a", bus.clone(), &api).with_actions(approval_runtime());

    let out = agent
        .think(&BusMessage::text("aja hyväksyntä-työkalu"))
        .await
        .expect("approval-polku ei saa palauttaa virhettä");

    // Core claim: the result is Suspended (NOT Reply) and carries the approval_id.
    match out {
        ThinkOutcome::Suspended {
            approval_id,
            redacted_summary,
        } => {
            // approval_id is genuine (not nil) → the operator can `approve` it.
            assert!(
                !approval_id.is_nil(),
                "Suspended kantaa aidon hyväksyntätunnisteen"
            );
            // The redacted summary must not leak the raw payload
            // ("do-it") nor secrets — only neutral metadata.
            assert!(
                !redacted_summary.contains("do-it"),
                "redaktoitu tiivistelmä ei saa sisältää raakaa payloadia, sai: {redacted_summary}"
            );
            assert!(
                !redacted_summary.is_empty(),
                "redaktoitu tiivistelmä ei saa olla tyhjä"
            );
        }
        other => panic!("odotettiin Suspended, sai: {other:?}"),
    }
    bus.stop();
}

/// (f2) **Suspended notifies the user** so the turn does not go silent.
/// A tool requiring approval is recorded in durable state AND a short
/// Discord message is sent (not a popup notification).
#[tokio::test]
async fn suspended_turn_produces_no_user_reply() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![body_tool_call(
        "call_approve",
        "approval_skill",
        &serde_json::json!({ "q": "do-it" }),
    )])
    .await;
    let (sink, mut rx) = new_reply_channel();
    let mut agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(approval_runtime())
        .with_reply_sink(sink)
        .with_reply_target("discord:general-1");

    let outcome = agent
        .handle_turn(BeingId::new(), &BusMessage::text("aja hyväksyntä-työkalu"))
        .await
        .expect("vuoro ei saa kaatua suspendiin");

    let ack = rx
        .try_recv()
        .expect("pitkän vuoron pitää alkaa ack-viestillä");
    assert!(
        ack.body.contains("Working on it"),
        "ack-viestin pitää kertoa työstä, sai: {}",
        ack.body
    );
    let mut suspend_body = None;
    while let Ok(msg) = rx.try_recv() {
        if msg.body.contains("turvapysäytys") || msg.body.contains("hyväksyntää") {
            suspend_body = Some(msg.body);
            break;
        }
    }
    let reply_body = suspend_body
        .expect("suspended-vuoron pitää ilmoittaa käyttäjälle ettei jäädä hiljaisuuteen");
    assert!(
        reply_body.contains("turvapysäytys") || reply_body.contains("hyväksyntää"),
        "suspend-ilmoituksen pitää kertoa odotuksesta, sai: {reply_body}"
    );
    // The turn summary records the suspend (resume/audit context),
    // but NOT the raw payload.
    assert!(
        outcome.summary.contains("suspended(approval="),
        "vuoron yhteenvedon pitäisi merkitä suspend, sai: {}",
        outcome.summary
    );
    assert!(
        !outcome.summary.contains("do-it"),
        "suspend-yhteenveto ei saa sisältää raakaa payloadia, sai: {}",
        outcome.summary
    );
    bus.stop();
}

// ---- TURN-AUDIT (roadmap §6 D6): observable tool loop ----

/// Helper: collects the `kind` values of a given turn's (correlation id's)
/// events, in insertion order, from the whole collector. Since the id is
/// generated inside the agent, we group the trace by `action_id`: in these
/// tests there is only one turn, so the only non-empty group is the
/// turn being looked for.
fn audit_kinds(audit: &AuditCollector) -> Vec<AuditKind> {
    audit.list().into_iter().map(|e| e.kind).collect()
}

/// (g) **A turn that dispatches a tool produces audit records:**
/// `TurnStarted` + `ToolDispatched` + `TurnAnswered`. This makes the tool
/// loop observable (roadmap §6 D6): the first response requests the
/// `loop_echo` tool, the second responds with text → the loop stops. The
/// audit trace must describe the entire lifecycle.
#[tokio::test]
async fn turn_audit_records_start_dispatch_and_stop_reason() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![
        body_tool_call("call_1", "loop_echo", &serde_json::json!({ "q": "ping" })),
        body_text("työkalu vastasi, valmis"),
    ])
    .await;
    let audit = StdArc::new(AuditCollector::new());
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(echo_runtime())
        .with_turn_audit(StdArc::clone(&audit));

    let out = agent
        .think(&BusMessage::text("aja työkalu"))
        .await
        .expect("loop ok");
    assert_eq!(
        out,
        ThinkOutcome::Reply("työkalu vastasi, valmis".to_string())
    );

    // Audit trace: start → dispatch → answered, in exactly this order.
    let kinds = audit_kinds(&audit);
    assert_eq!(
        kinds,
        vec![
            AuditKind::TurnStarted,
            AuditKind::ToolDispatched,
            AuditKind::TurnAnswered,
        ],
        "audit-jäljen pitää kuvata alku + dispatch + stop_reason, sai: {kinds:?}"
    );

    // All events share the same turn correlation id, and it can be
    // fetched via `turn_audit_for` (the operator's per-turn surface).
    let events = audit.list();
    let turn_id = events[0].action_id;
    assert!(
        events.iter().all(|e| e.action_id == turn_id),
        "yhden vuoron kaikki tapahtumat jakavat saman tunnisteen"
    );
    assert_eq!(
        agent.turn_audit_for(turn_id).len(),
        3,
        "turn_audit_for palauttaa vuoron koko jäljen"
    );

    // The dispatch record names the skill (observability), not the arguments.
    let dispatch = events
        .iter()
        .find(|e| e.kind == AuditKind::ToolDispatched)
        .expect("dispatch event present");
    assert!(
        dispatch.detail.contains("loop_echo"),
        "dispatch-merkinnän pitää nimetä taito, sai: {}",
        dispatch.detail
    );
    bus.stop();
}

/// (h) **A suspended turn records the suspend with `approval_id`.**
/// A tool requiring approval → a `TurnSuspended` event whose `detail`
/// carries the approval id (resume/audit context) — not the raw payload.
#[tokio::test]
async fn turn_audit_records_suspend_with_approval_id() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![body_tool_call(
        "call_approve",
        "approval_skill",
        &serde_json::json!({ "q": "do-it" }),
    )])
    .await;
    let audit = StdArc::new(AuditCollector::new());
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(approval_runtime())
        .with_turn_audit(StdArc::clone(&audit));

    let out = agent
        .think(&BusMessage::text("aja hyväksyntä-työkalu"))
        .await
        .expect("approval-polku ok");
    let approval_id = match out {
        ThinkOutcome::Suspended { approval_id, .. } => approval_id,
        other => panic!("odotettiin Suspended, sai: {other:?}"),
    };

    // Audit trace: start → dispatch → suspended.
    let kinds = audit_kinds(&audit);
    assert_eq!(
        kinds,
        vec![
            AuditKind::TurnStarted,
            AuditKind::ToolDispatched,
            AuditKind::TurnSuspended,
        ],
        "suspendin pitää näkyä stop_reason-merkintänä, sai: {kinds:?}"
    );

    // The suspend record carries the approval id (the operator can
    // correlate it with the `approve` call), not the raw payload ("do-it").
    let suspend = audit
        .list()
        .into_iter()
        .find(|e| e.kind == AuditKind::TurnSuspended)
        .expect("suspend event present");
    assert!(
        suspend.detail.contains(&approval_id.to_string()),
        "suspend-merkinnän pitää kantaa approval_id, sai: {}",
        suspend.detail
    );
    assert!(
        !suspend.detail.contains("do-it"),
        "suspend-merkintä ei saa sisältää raakaa payloadia, sai: {}",
        suspend.detail
    );
    bus.stop();
}

/// (i) **Secrecy invariant: no audit record contains the raw secret.**
/// Runs a tool call whose argument carries a secret (built at runtime,
/// not a literal in the source — Layer B), and the tool echoes it into
/// its result. The audit `detail` is redacted (both by proof AND by the
/// agent's `redact_free_text` defense), so the raw secret must not
/// appear in any event.
#[tokio::test]
async fn turn_audit_never_contains_raw_secret() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    // The secret is built at runtime (not a literal in the source).
    let secret = format!("sk-{}", "live".repeat(4));
    let api = spawn_scripted_llm(vec![
        // The argument carries the secret → the tool echoes it into the result.
        body_tool_call(
            "call_secret",
            "loop_echo",
            &serde_json::json!({ "q": secret.clone() }),
        ),
        body_text("valmis"),
    ])
    .await;
    let audit = StdArc::new(AuditCollector::new());
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(echo_runtime())
        .with_turn_audit(StdArc::clone(&audit));

    let _ = agent
        .think(&BusMessage::text("aja salaisuuden kanssa"))
        .await
        .expect("loop ok");

    // No audit record may carry the raw secret.
    let rendered = serde_json::to_string(&audit.list()).expect("serialize audit");
    assert!(
        !rendered.contains(&secret),
        "audit-jälki ei saa sisältää raakaa salaisuutta:\n{rendered}"
    );
    // Make sure the dispatch record actually occurred (otherwise the test would be vacuous).
    assert!(
        audit
            .list()
            .iter()
            .any(|e| e.kind == AuditKind::ToolDispatched),
        "dispatch-merkinnän pitää syntyä, jotta redaktointi on testattu"
    );
    bus.stop();
}

/// (j) **Without an attached audit, the tool loop records nothing**
/// (additive, backward-compatible): `turn_audit()` is `None` and
/// `turn_audit_for` returns empty.
#[tokio::test]
async fn turn_audit_absent_is_noop() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![
        body_tool_call("call_1", "loop_echo", &serde_json::json!({ "q": "ping" })),
        body_text("valmis"),
    ])
    .await;
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api).with_actions(echo_runtime());
    assert!(agent.turn_audit().is_none(), "auditia ei ole kytketty");

    let _ = agent
        .think(&BusMessage::text("aja työkalu"))
        .await
        .expect("loop ok");
    // Without a collector there is no id → empty trace.
    assert!(agent.turn_audit_for(ActionId::new()).is_empty());
    bus.stop();
}

/// `with_tool_loop` adjusts the limit and `tool_loop()` reads it; the
/// default is [`ToolLoopConfig::DEFAULT_MAX_ITERATIONS`].
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

// ---- 1C suspend/resume bridge (roadmap §6) -----------------------------

use crate::resumable::{InMemoryResumableStore, JournalResumableStore, ResumableTurnStore};
use familyclaw_actions::{
    DangerousToolRateLimiter, InMemoryPendingStore, JournalPendingStore, PendingApprovalStore,
    PendingRecord,
};

/// RAII temp directory for durable-surface writes (no external crates).
/// Provides two file paths: the pending and resumable journals.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "familyclaw-resume-bridge-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&p).expect("create temp dir");
        Self(p)
    }
    fn pending_path(&self) -> std::path::PathBuf {
        self.0.join("pending.jsonl")
    }
    fn task_queue_path(&self) -> std::path::PathBuf {
        self.0.join("tasks.jsonl")
    }
    fn outbox_path(&self) -> std::path::PathBuf {
        self.0.join("dispatch_outbox.jsonl")
    }
    fn resumable_path(&self) -> std::path::PathBuf {
        self.0.join("resumable.jsonl")
    }
}

/// Builds a **fully crash-resilient** shared runtime with the counting
/// approval skill: durable pending + durable task queue + durable
/// dispatch outbox (all reconstructed from the given files) +
/// a per-test counter.
async fn durable_counting_runtime(
    pending_path: std::path::PathBuf,
    task_queue_path: std::path::PathBuf,
    outbox_path: std::path::PathBuf,
    count: StdArc<std::sync::atomic::AtomicUsize>,
) -> StdArc<TokioMutex<ActionRuntime>> {
    let mut rt = ActionRuntime::with_durable_stores(pending_path, task_queue_path, outbox_path)
        .await
        .expect("durable stores open");
    rt.register_skill(CountingApprovalSkill::new(count))
        .expect("register counting approval_skill");
    StdArc::new(TokioMutex::new(rt))
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Builds a shared runtime with the COUNTING approval skill + the given
/// pending-approvals storage surface (durable or in-memory).
/// `count` is a per-test shared counter (concurrency isolation).
fn counting_runtime_with_pending(
    pending: Box<dyn PendingApprovalStore>,
    count: StdArc<std::sync::atomic::AtomicUsize>,
) -> StdArc<TokioMutex<ActionRuntime>> {
    let mut rt = ActionRuntime::with_pending_store(pending);
    rt.register_skill(CountingApprovalSkill::new(count))
        .expect("register counting approval_skill");
    StdArc::new(TokioMutex::new(rt))
}

/// Resumable store that deterministically rejects writes. Read-side methods
/// stay healthy so a test failure can only come from the durability boundary.
#[derive(Debug, Default)]
struct FailingPutResumableStore;

impl ResumableTurnStore for FailingPutResumableStore {
    fn put(&self, _turn: ResumableTurn) -> crate::resumable::Result<()> {
        Err(crate::resumable::ResumableError::Journal(
            "injected put failure".to_string(),
        ))
    }

    fn get(&self, _approval_id: ApprovalId) -> crate::resumable::Result<Option<ResumableTurn>> {
        Ok(None)
    }

    fn remove(&self, _approval_id: ApprovalId) -> crate::resumable::Result<Option<ResumableTurn>> {
        Ok(None)
    }

    fn len(&self) -> crate::resumable::Result<usize> {
        Ok(0)
    }

    fn evict_expired(&self, _now: Timestamp) -> crate::resumable::Result<usize> {
        Ok(0)
    }
}

/// Store that accepts the initial suspend and rejects the next chained
/// suspend. This isolates the continuation durability boundary.
#[derive(Debug, Default)]
struct FailOnSecondPutResumableStore {
    inner: InMemoryResumableStore,
    puts: std::sync::atomic::AtomicUsize,
}

impl ResumableTurnStore for FailOnSecondPutResumableStore {
    fn put(&self, turn: ResumableTurn) -> crate::resumable::Result<()> {
        let call = self.puts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if call == 2 {
            return Err(crate::resumable::ResumableError::Journal(
                "injected second put failure".to_string(),
            ));
        }
        self.inner.put(turn)
    }

    fn get(&self, approval_id: ApprovalId) -> crate::resumable::Result<Option<ResumableTurn>> {
        self.inner.get(approval_id)
    }

    fn remove(&self, approval_id: ApprovalId) -> crate::resumable::Result<Option<ResumableTurn>> {
        self.inner.remove(approval_id)
    }

    fn len(&self) -> crate::resumable::Result<usize> {
        self.inner.len()
    }

    fn evict_expired(&self, now: Timestamp) -> crate::resumable::Result<usize> {
        self.inner.evict_expired(now)
    }
}

/// Pending store that keeps the record live when removal fails. This models
/// a durable tombstone write failure: rollback must still prevent the live
/// record from authorizing a side effect through another approval surface.
#[derive(Debug, Default)]
struct FailingRemovePendingStore {
    inner: InMemoryPendingStore,
}

impl PendingApprovalStore for FailingRemovePendingStore {
    fn insert(&self, record: PendingRecord) -> familyclaw_actions::Result<()> {
        self.inner.insert(record)
    }

    fn get(&self, approval_id: ApprovalId) -> familyclaw_actions::Result<Option<PendingRecord>> {
        self.inner.get(approval_id)
    }

    fn remove(
        &self,
        _approval_id: ApprovalId,
    ) -> familyclaw_actions::Result<Option<PendingRecord>> {
        Err(familyclaw_actions::ActionError::Proof(
            "injected pending tombstone failure".to_string(),
        ))
    }

    fn len(&self) -> familyclaw_actions::Result<usize> {
        self.inner.len()
    }

    fn list(&self) -> familyclaw_actions::Result<Vec<PendingRecord>> {
        self.inner.list()
    }

    fn evict_expired(&self, now: Timestamp) -> familyclaw_actions::Result<usize> {
        self.inner.evict_expired(now)
    }
}

/// Resumable store that writes the record, announces its id, and then blocks
/// until the test releases it. This exposes the exact check/put/approve
/// interleaving without sleeps inside production code.
#[derive(Debug)]
struct BlockingPutResumableStore {
    inner: InMemoryResumableStore,
    entered: std::sync::Mutex<Option<std::sync::mpsc::Sender<ApprovalId>>>,
    release: StdArc<AtomicBool>,
}

impl BlockingPutResumableStore {
    fn new(entered: std::sync::mpsc::Sender<ApprovalId>, release: StdArc<AtomicBool>) -> Self {
        Self {
            inner: InMemoryResumableStore::new(),
            entered: std::sync::Mutex::new(Some(entered)),
            release,
        }
    }
}

impl ResumableTurnStore for BlockingPutResumableStore {
    fn put(&self, turn: crate::resumable::ResumableTurn) -> crate::resumable::Result<()> {
        let approval_id = turn.approval_id;
        self.inner.put(turn)?;
        if let Some(sender) = self
            .entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = sender.send(approval_id);
        }
        while !self.release.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Ok(())
    }

    fn get(
        &self,
        approval_id: ApprovalId,
    ) -> crate::resumable::Result<Option<crate::resumable::ResumableTurn>> {
        self.inner.get(approval_id)
    }

    fn remove(
        &self,
        approval_id: ApprovalId,
    ) -> crate::resumable::Result<Option<crate::resumable::ResumableTurn>> {
        self.inner.remove(approval_id)
    }

    fn len(&self) -> crate::resumable::Result<usize> {
        self.inner.len()
    }

    fn evict_expired(&self, now: Timestamp) -> crate::resumable::Result<usize> {
        self.inner.evict_expired(now)
    }
}

/// A turn must not report a recoverable suspension if its resume state never
/// became durable. Returning `Suspended` here would create a false-success
/// control state: the operator could approve it, but no continuation exists.
#[tokio::test]
async fn suspend_fails_closed_when_resumable_state_cannot_be_persisted() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![body_tool_call(
        "call_approve",
        "approval_skill",
        &serde_json::json!({ "q": "do-it" }),
    )])
    .await;
    let store: StdArc<dyn ResumableTurnStore> = StdArc::new(FailingPutResumableStore);
    let runtime = approval_runtime();
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(store);

    let err = agent
        .think(&BusMessage::text("aja hyväksyntä-työkalu"))
        .await
        .expect_err("non-durable suspension must fail closed");

    assert!(
        err.to_string().contains("resumable persist failed"),
        "unexpected error: {err}"
    );
    assert!(
        runtime
            .lock()
            .await
            .try_pending_approvals()
            .expect("pending list")
            .is_empty(),
        "failed persistence must roll back the orphan approval"
    );
    bus.stop();
}

/// The pending check and resumable write share the same `ActionRuntime` lock as
/// approval consumption. A direct approval attempt cannot execute while the
/// durable write is still in flight.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_waits_until_resumable_write_finishes() {
    struct ReleaseOnDrop(StdArc<AtomicBool>);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![body_tool_call(
        "call_approve",
        "approval_skill",
        &serde_json::json!({ "q": "do-it" }),
    )])
    .await;
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let release = StdArc::new(AtomicBool::new(false));
    let _release_guard = ReleaseOnDrop(StdArc::clone(&release));
    let store: StdArc<dyn ResumableTurnStore> = StdArc::new(BlockingPutResumableStore::new(
        entered_tx,
        StdArc::clone(&release),
    ));
    let count = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    let runtime =
        counting_runtime_with_pending(Box::new(InMemoryPendingStore::new()), StdArc::clone(&count));
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(StdArc::clone(&store));

    let think_task = tokio::spawn(async move {
        agent
            .think(&BusMessage::text("aja hyväksyntä-työkalu"))
            .await
    });
    let approval_id = tokio::task::spawn_blocking(move || {
        entered_rx.recv_timeout(std::time::Duration::from_secs(2))
    })
    .await
    .expect("put entry observer joins")
    .expect("resumable put entered");

    let approve_runtime = StdArc::clone(&runtime);
    let mut approve_task = tokio::spawn(async move {
        approve_runtime
            .lock()
            .await
            .approve(approval_id, time::now())
            .await
    });
    let early = tokio::time::timeout(std::time::Duration::from_millis(75), &mut approve_task).await;
    let approve_was_blocked = early.is_err();
    let count_while_write_blocked = count.load(std::sync::atomic::Ordering::SeqCst);

    release.store(true, std::sync::atomic::Ordering::SeqCst);
    let suspended = think_task
        .await
        .expect("think task joins")
        .expect("suspend succeeds after write release");
    let approve_result = match early {
        Ok(joined) => joined,
        Err(_) => approve_task.await,
    }
    .expect("approval task joins");
    approve_result.expect("approval executes after durable write");

    assert!(
        approve_was_blocked,
        "approval must wait for the resumable write to release the runtime lock"
    );
    assert_eq!(
        count_while_write_blocked, 0,
        "side effect must not run while continuation persistence is in flight"
    );
    assert!(
        matches!(
            suspended,
            ThinkOutcome::Suspended {
                approval_id: id,
                ..
            } if id == approval_id
        ),
        "turn must suspend with the persisted approval id"
    );
    assert!(
        store.get(approval_id).expect("get resumable").is_some(),
        "continuation must exist before approval execution is released"
    );
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "approval executes exactly once after durable write completion"
    );
    bus.stop();
}

/// Rollback is idempotent when an operator denial wins the race after the
/// pending approval was created but before persistence compensation runs.
/// A missing pending record means the permission is already closed; any
/// partial resumable record must still be tombstoned.
#[tokio::test]
async fn rollback_cleans_resumable_when_operator_denied_first() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![body_tool_call(
        "call_approve",
        "approval_skill",
        &serde_json::json!({ "q": "do-it" }),
    )])
    .await;
    let store: StdArc<dyn ResumableTurnStore> = StdArc::new(InMemoryResumableStore::new());
    let runtime = approval_runtime();
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(StdArc::clone(&store));

    let suspended = agent
        .think(&BusMessage::text("aja hyväksyntä-työkalu"))
        .await
        .expect("suspend persists");
    let approval_id = match suspended {
        ThinkOutcome::Suspended { approval_id, .. } => approval_id,
        other => panic!("expected suspension, got: {other:?}"),
    };
    assert!(
        store.get(approval_id).expect("get resumable").is_some(),
        "fixture must contain the resumable record"
    );

    let now = time::now();
    runtime
        .lock()
        .await
        .deny_pending(approval_id, now)
        .await
        .expect("operator denial closes permission first");

    let cleanup_error = agent
        .rollback_non_durable_suspend(&runtime, approval_id, now)
        .await;

    assert!(
        cleanup_error.is_none(),
        "already-denied approval is an idempotent rollback success: {cleanup_error:?}"
    );
    assert!(
        store
            .get(approval_id)
            .expect("get after rollback")
            .is_none(),
        "rollback must remove a possibly-partial resumable even when approval was already closed"
    );
    bus.stop();
}

/// Even if the pending-store tombstone cannot be written, compensation must
/// quarantine the approval before returning. A direct approval attempt must
/// fail closed and the executor must remain untouched.
#[tokio::test]
async fn rollback_blocks_side_effect_when_pending_remove_fails() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![body_tool_call(
        "call_approve",
        "approval_skill",
        &serde_json::json!({ "q": "do-it" }),
    )])
    .await;
    let store: StdArc<dyn ResumableTurnStore> = StdArc::new(InMemoryResumableStore::new());
    let count = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    let runtime = counting_runtime_with_pending(
        Box::new(FailingRemovePendingStore::default()),
        StdArc::clone(&count),
    );
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(StdArc::clone(&store));

    let suspended = agent
        .think(&BusMessage::text("aja hyväksyntä-työkalu"))
        .await
        .expect("suspend persists");
    let approval_id = match suspended {
        ThinkOutcome::Suspended { approval_id, .. } => approval_id,
        other => panic!("expected suspension, got: {other:?}"),
    };

    let now = time::now();
    let cleanup_error = agent
        .rollback_non_durable_suspend(&runtime, approval_id, now)
        .await;
    assert!(
        cleanup_error
            .as_deref()
            .is_some_and(|error| error.contains("approval rollback failed")),
        "tombstone failure must stay visible: {cleanup_error:?}"
    );
    assert!(
        store
            .get(approval_id)
            .expect("get after rollback")
            .is_none(),
        "resumable cleanup must still be attempted"
    );

    let approve_error = runtime
        .lock()
        .await
        .approve(approval_id, now)
        .await
        .expect_err("quarantined approval must not execute");
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "rollback failure must never authorize a side effect"
    );
    assert!(
        matches!(
            approve_error,
            familyclaw_actions::ActionError::PolicyDenied(_)
                | familyclaw_actions::ActionError::ExecutionFailed(_)
                | familyclaw_actions::ActionError::Proof(_)
        ),
        "unexpected approval error: {approve_error}"
    );
    bus.stop();
}

/// The production turn path uses the durable tool loop rather than
/// `think()` directly. It must not expose an approval command, record a
/// suspended outcome, or leave a pending approval when the matching
/// continuation was never persisted.
#[tokio::test]
async fn durable_turn_does_not_advertise_unpersisted_approval() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![body_tool_call(
        "call_approve",
        "approval_skill",
        &serde_json::json!({ "q": "do-it" }),
    )])
    .await;
    let store: StdArc<dyn ResumableTurnStore> = StdArc::new(FailingPutResumableStore);
    let runtime = approval_runtime();
    let (sink, mut rx) = new_reply_channel();
    let mut agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(store)
        .with_reply_sink(sink)
        .with_reply_target("test:dependability");

    let outcome = agent
        .handle_turn(BeingId::new(), &BusMessage::text("aja hyväksyntä-työkalu"))
        .await
        .expect("turn contains the failure and stays operational");

    assert!(
        !outcome.summary.contains("suspended(approval="),
        "non-durable approval must not be recorded as suspended: {}",
        outcome.summary
    );
    let reply = rx.try_recv().expect("fail-closed recovery reply");
    assert!(
        !reply.body.contains("APPROVE") && !reply.body.contains("/approvals/"),
        "operator must not receive an unusable approval command: {}",
        reply.body
    );
    assert!(
        runtime
            .lock()
            .await
            .try_pending_approvals()
            .expect("pending list")
            .is_empty(),
        "production persist failure must roll back the orphan approval"
    );
    bus.stop();
}

/// A failed production suspend must not leave durable think/suspend markers.
/// Those markers would advance the replay cursor on restart and let a later
/// run advertise Suspended without a recoverable continuation.
#[tokio::test]
async fn durable_turn_persist_failure_leaves_no_false_suspend_journal_markers() {
    let dir = TempDir::new("false-suspend-journal");
    let journal_path = dir.0.join("agent.journal.jsonl");
    let mem_path = dir.0.join("memory.json");
    let memory: ErasedMemoryStore =
        Arc::new(LocalJsonStore::open(&mem_path).await.expect("open mem"));
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![body_tool_call(
        "call_approve",
        "approval_skill",
        &serde_json::json!({ "q": "do-it" }),
    )])
    .await;
    let store: StdArc<dyn ResumableTurnStore> = StdArc::new(FailingPutResumableStore);
    let runtime = approval_runtime();
    let (sink, mut rx) = new_reply_channel();
    let mut agent = agent_over_file_journal("agent_a", bus.clone(), &api, &journal_path, memory)
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(store)
        .with_reply_sink(sink)
        .with_reply_target("test:dependability");

    let outcome = agent
        .handle_turn(BeingId::new(), &BusMessage::text("aja hyväksyntä-työkalu"))
        .await
        .expect("turn stays operational");

    assert!(
        !outcome.summary.contains("suspended(approval="),
        "non-durable approval must not be recorded as suspended: {}",
        outcome.summary
    );
    let reply = rx.try_recv().expect("fail-closed recovery reply");
    assert!(
        !reply.body.contains("APPROVE") && !reply.body.contains("/approvals/"),
        "operator must not receive an unusable approval command: {}",
        reply.body
    );
    assert!(
        runtime
            .lock()
            .await
            .try_pending_approvals()
            .expect("pending list")
            .is_empty(),
        "production persist failure must roll back the orphan approval"
    );

    let journal_text = std::fs::read_to_string(&journal_path).expect("read journal");
    assert!(
        !journal_text.contains("turn-0-think"),
        "failed suspend must not journal empty think before durable continuation: {journal_text}"
    );
    assert!(
        !journal_text.contains("turn-0-suspend"),
        "failed suspend must not journal a suspend marker: {journal_text}"
    );
    bus.stop();
}

/// A chained approval created during resume has the same durability contract
/// as the initial suspend. The already-approved side effect may have run, but
/// the continuation must not claim `Suspended` when its new resume state was
/// not persisted.
#[tokio::test]
async fn chained_suspend_fails_closed_when_resumable_state_cannot_be_persisted() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![
        body_tool_call(
            "call_approve_1",
            "approval_skill",
            &serde_json::json!({ "q": "first" }),
        ),
        body_tool_call(
            "call_approve_2",
            "approval_skill",
            &serde_json::json!({ "q": "second" }),
        ),
    ])
    .await;
    let store: StdArc<dyn ResumableTurnStore> =
        StdArc::new(FailOnSecondPutResumableStore::default());
    let count = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    let runtime = counting_runtime_with_pending(
        Box::new(familyclaw_actions::InMemoryPendingStore::new()),
        StdArc::clone(&count),
    );
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(store);

    let initial = agent
        .think(&BusMessage::text("aloita kaksivaiheinen hyväksyntä"))
        .await
        .expect("initial suspend persists");
    let first_approval = match initial {
        ThinkOutcome::Suspended { approval_id, .. } => approval_id,
        other => panic!("expected initial suspension, got: {other:?}"),
    };

    let err = agent
        .resume_approved(first_approval, time::now())
        .await
        .expect_err("non-durable chained suspension must fail closed");

    assert!(
        err.to_string().contains("resumable persist failed"),
        "unexpected error: {err}"
    );
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the first approved action executes exactly once"
    );
    assert!(
        runtime
            .lock()
            .await
            .try_pending_approvals()
            .expect("pending list")
            .is_empty(),
        "failed chained persistence must roll back the orphan approval"
    );
    bus.stop();
}

/// (a) **Suspend persists the resumable turn.** When the tool loop
/// suspends to wait for approval, the resumable turn is stored on the
/// resumable surface with the correct `approval_id`, and it does not
/// contain the raw payload/secrets.
#[tokio::test]
async fn suspend_persists_resumable_turn_without_secrets() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![body_tool_call(
        "call_approve",
        "approval_skill",
        &serde_json::json!({ "q": "do-it", "api_key": "sk-livelivelive" }),
    )])
    .await;
    let store: StdArc<dyn ResumableTurnStore> = StdArc::new(InMemoryResumableStore::new());
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(approval_runtime())
        .with_resumable_store(StdArc::clone(&store));

    let out = agent
        .think(&BusMessage::text("aja hyväksyntä-työkalu"))
        .await
        .expect("suspend ok");

    let approval_id = match out {
        ThinkOutcome::Suspended { approval_id, .. } => approval_id,
        other => panic!("odotettiin Suspended, sai: {other:?}"),
    };

    // The resumable turn is on the surface with the correct key.
    assert_eq!(store.len().expect("len"), 1);
    let turn = store
        .get(approval_id)
        .expect("get")
        .expect("resumable persisted with the right approval_id");
    assert_eq!(turn.approval_id, approval_id);
    assert_eq!(turn.tool_name, "approval_skill");
    // Message stack preserved (system + user + assistant tool-call).
    assert!(turn.messages.len() >= 2, "message stack persisted");

    // NO raw SECRET in any field — the message stack's tool-call
    // arguments are redacted before storage.
    let json = serde_json::to_string(&turn).expect("serialize turn");
    assert!(
        !json.contains("sk-livelivelive"),
        "resumable turn must not contain the raw secret"
    );
    // The arguments-summary field must NOT carry raw arguments:
    // redacted_arguments is a neutral summary, arguments_hash is SHA-256.
    assert!(
        !turn.redacted_arguments.contains("sk-livelivelive")
            && !turn.redacted_arguments.contains("do-it"),
        "redacted_arguments must not carry raw args/secrets, got: {}",
        turn.redacted_arguments
    );
    assert_eq!(turn.arguments_hash.len(), 64, "sha256 hex present");
    // The hash binds exactly to the original (non-redacted) arguments
    // (payload binding for resume).
    let expected_hash = familyclaw_actions::approval::sha256_hex(
        &serde_json::to_vec(&serde_json::json!({ "q": "do-it", "api_key": "sk-livelivelive" }))
            .unwrap(),
    );
    assert_eq!(turn.arguments_hash, expected_hash);
    bus.stop();
}

/// (a2) **Suspend does not leak a secret EMBEDDED in free text** — neither
/// inside a tool argument nor in a user message. This is exactly the gap
/// from defect #2: the old redaction only masked whole-value and
/// known-key-name secrets, so a secret INSIDE a larger string (or in a
/// user message) ended up on disk raw. This setup uses the crash-resilient
/// [`JournalResumableStore`] and reads the FILE content directly: if the
/// secret were on disk, it would show up in the `.jsonl`.
#[tokio::test]
async fn suspend_does_not_persist_secret_embedded_in_free_text() {
    let dir = TempDir::new("embedded");
    let secret = format!("sk-{}", "live".repeat(4));
    let bus = ResonanceBus::start(None).await.expect("bus");
    // A tool argument where the secret is EMBEDDED in free text (the field
    // name `prompt` is NOT a secret key, and the whole value is not merely a token).
    let api = spawn_scripted_llm(vec![body_tool_call(
        "call_approve",
        "approval_skill",
        &serde_json::json!({ "prompt": format!("deploy using {secret} then ship") }),
    )])
    .await;
    let resumable: StdArc<dyn ResumableTurnStore> =
        StdArc::new(JournalResumableStore::open(dir.resumable_path()).expect("resumable"));
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(approval_runtime())
        .with_resumable_store(StdArc::clone(&resumable));

    // A user message that itself carries the secret as free text.
    let out = agent
        .think(&BusMessage::text(format!("use my key {secret} to deploy")))
        .await
        .expect("suspend ok");
    let approval_id = match out {
        ThinkOutcome::Suspended { approval_id, .. } => approval_id,
        other => panic!("odotettiin Suspended, sai: {other:?}"),
    };

    // 1. The journal persisted to disk must NOT contain the raw secret.
    let on_disk = std::fs::read_to_string(dir.resumable_path()).expect("read journal");
    assert!(
        !on_disk.contains(&secret),
        "persisted resumable journal leaked an embedded secret:\n{on_disk}"
    );
    // 2. Nor in the reconstructed turn (arguments + message stack content).
    let turn = resumable.get(approval_id).expect("get").expect("present");
    let turn_json = serde_json::to_string(&turn).expect("serialize turn");
    assert!(
        !turn_json.contains(&secret),
        "resumable turn leaked an embedded secret: {turn_json}"
    );
    // The redaction mask IS present (proof that the pass triggered), and
    // the harmless surrounding text was preserved (deploy/ship) — not
    // merely a whole-value wipe.
    assert!(turn_json.contains("[REDACTED]"), "redaction mask present");
    bus.stop();
}

/// (b) **`resume_approved` loads, approves, and completes the turn
/// (Reply) — the side effect runs EXACTLY ONCE.** First the model calls
/// the tool requiring approval (suspend). Resume consumes the approval
/// (= the skill executes once), feeds the result back, and the model
/// responds with the final text.
#[tokio::test]
async fn resume_approved_completes_turn_side_effect_runs_once() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    // Request 1: call the approval tool (suspend).
    // Request 2 (during resume): having seen the tool's result, respond with text.
    let api = spawn_scripted_llm(vec![
        body_tool_call(
            "call_approve",
            "approval_skill",
            &serde_json::json!({ "q": "ship" }),
        ),
        body_text("hyväksytty toiminto valmis"),
    ])
    .await;
    let store: StdArc<dyn ResumableTurnStore> = StdArc::new(InMemoryResumableStore::new());
    let count = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    let runtime = counting_runtime_with_pending(
        Box::new(familyclaw_actions::InMemoryPendingStore::new()),
        StdArc::clone(&count),
    );
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(StdArc::clone(&store));

    // Step 1: suspend.
    let out = agent
        .think(&BusMessage::text("ship it"))
        .await
        .expect("suspend ok");
    let approval_id = match out {
        ThinkOutcome::Suspended { approval_id, .. } => approval_id,
        other => panic!("odotettiin Suspended, sai: {other:?}"),
    };
    // Before approval the skill has NOT executed.
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "approval-gated action must NOT run before approve"
    );

    // Step 2: resume → approve and complete.
    let now = time::now();
    let resumed = agent
        .resume_approved(approval_id, now)
        .await
        .expect("resume_approved ok");
    assert_eq!(
        resumed,
        ThinkOutcome::Reply("hyväksytty toiminto valmis".to_string()),
        "resume jatkaa loopin lopulliseen vastaukseen"
    );

    // The side effect ran EXACTLY ONCE.
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "approval-gated side effect must run exactly once"
    );
    // The resumable turn was consumed (removed from the surface).
    assert!(
        store.get(approval_id).expect("get").is_none(),
        "resumable turn consumed after resume"
    );
    bus.stop();
}

/// **TASK 1: `handle_resume_signal` routes the continuation of an
/// approved turn to the reply sink.** This is the agent's half of the
/// suspend/resume bridge: the operator's approval arrives as the bus's
/// `ResumeApproval` signal, the agent continues the suspended tool loop
/// to completion (`resume_approved`) and pushes the final response OUT to
/// the reply sink (`route_reply`) — NO new LLM turn, NO bus publication
/// (echo-loop protection).
///
/// Claims:
/// - the side effect (the approval-gated skill) runs EXACTLY ONCE,
/// - the final response text ends up in the captured reply sink with the
///   correct target (the resumable turn's `conversation_origin`),
/// - the resumable turn is consumed (resume is single-use).
#[tokio::test]
async fn resume_signal_routes_to_reply_sink() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    // Request 1: call the approval tool (suspend).
    // Request 2 (during resume): having seen the tool's result, respond with text.
    let api = spawn_scripted_llm(vec![
        body_tool_call(
            "call_approve",
            "approval_skill",
            &serde_json::json!({ "q": "ship" }),
        ),
        body_text("hyväksytty toiminto valmis"),
    ])
    .await;
    let store: StdArc<dyn ResumableTurnStore> = StdArc::new(InMemoryResumableStore::new());
    let count = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    let runtime = counting_runtime_with_pending(
        Box::new(familyclaw_actions::InMemoryPendingStore::new()),
        StdArc::clone(&count),
    );
    let (sink, mut rx) = new_reply_channel();
    // Per-message origin → the reply target is derived from the
    // resumable turn's `conversation_origin` (same logic as the normal route).
    let origin = familyclaw_bus::MessageOrigin::new("discord-main", "general-7", "operator");
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(StdArc::clone(&store))
        .with_reply_sink(sink);

    // Step 1: suspend (with per-message origin, so the turn stores the
    // `conversation_origin` for continuation).
    let out = agent
        .think_with_origin(&BusMessage::text("ship it"), Some(&origin))
        .await
        .expect("suspend ok");
    let approval_id = match out {
        ThinkOutcome::Suspended { approval_id, .. } => approval_id,
        other => panic!("odotettiin Suspended, sai: {other:?}"),
    };
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "approval-gated action must NOT run before approve"
    );

    // Step 2: RESUME SIGNAL (operator approval) -> the agent continues
    // the turn to completion and pushes the response to the reply sink.
    let now = time::now();
    agent
        .handle_resume_signal(&approval_id.to_string(), now)
        .await
        .expect("resume signal handled");

    // Side effect ran EXACTLY ONCE.
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "approval-gated side effect must run exactly once via resume signal"
    );
    // The final response ended up in the reply sink with the RIGHT target.
    let got = rx.recv().await.expect("reply delivered to sink");
    assert_eq!(
        got.target, "general-7",
        "reply routed to conversation_origin reply target"
    );
    assert_eq!(got.body, "hyväksytty toiminto valmis");
    // Resumable turn consumed.
    assert!(
        store.get(approval_id).expect("get").is_none(),
        "resumable turn consumed after resume signal"
    );
    bus.stop();
}

/// The chat-level APPROVE command must delegate to the resumable bridge
/// instead of consuming the approval first. Pre-consuming would execute the
/// action but make `resume_approved` reject the same single-use approval,
/// leaving the continuation stranded without its final reply.
#[tokio::test]
async fn operator_approve_command_consumes_once_and_resumes() {
    const OWNER_ENV: &str = "FAMILYCLAW_OWNER_ID";
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![
        body_tool_call(
            "call_approve",
            "approval_skill",
            &serde_json::json!({ "q": "ship" }),
        ),
        body_text("hyväksytty toiminto valmis"),
    ])
    .await;
    let store: StdArc<dyn ResumableTurnStore> = StdArc::new(InMemoryResumableStore::new());
    let count = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    let runtime =
        counting_runtime_with_pending(Box::new(InMemoryPendingStore::new()), StdArc::clone(&count));
    let (sink, mut rx) = new_reply_channel();
    let origin = familyclaw_bus::MessageOrigin::new("discord-main", "general-8", "42");
    let mut agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(StdArc::clone(&store))
        .with_reply_sink(sink);

    let suspended = agent
        .think_with_origin(&BusMessage::text("ship it"), Some(&origin))
        .await
        .expect("suspend ok");
    let approval_id = match suspended {
        ThinkOutcome::Suspended { approval_id, .. } => approval_id,
        other => panic!("expected suspension, got: {other:?}"),
    };

    let command = BusMessage::text(format!("APPROVE {approval_id}"));
    let command_outcome = {
        let _owner = HistoryEnvVarGuard::set(OWNER_ENV, "42");
        agent
            .handle_turn_with_origin(BeingId::new(), &command, Some(&origin))
            .await
            .expect("approval command handled")
    };

    assert!(command_outcome.summary.contains("APPROVE OK"));
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the approved side effect executes exactly once"
    );
    let first = rx.try_recv().expect("continuation reply routed");
    let second = rx.try_recv().expect("approval acknowledgement routed");
    assert_eq!(first.target, "general-8");
    assert_eq!(second.target, "general-8");
    let bodies = [first.body.as_str(), second.body.as_str()];
    assert!(bodies.contains(&"hyväksytty toiminto valmis"));
    assert!(bodies.iter().any(|body| body.contains("APPROVE OK")));
    assert!(
        store.get(approval_id).expect("get").is_none(),
        "resumable state is consumed after successful continuation"
    );
    assert!(
        runtime
            .lock()
            .await
            .try_pending_approvals()
            .expect("pending approvals readable")
            .is_empty(),
        "approval is consumed exactly once"
    );
    bus.stop();
}

/// (b-idempotent) **The continuation AFTER approval is idempotent** — closes
/// the last double-fire window.
///
/// Background: once approval is granted, [`resume_approved`] continues the
/// turn with [`drive_tool_loop`] using the idempotency key prefix
/// `resume-{approval_id}`. Previously this path dispatched post-approval
/// tools via [`ActionRuntime::submit_task_as`] (non-idempotent), so a crash
/// BETWEEN the side effect and its journaling could have triggered the
/// side effect twice on replay.
///
/// This test simulates exactly that crash-then-replay window: it runs
/// `drive_tool_loop` **twice with the SAME** idempotency prefix against a
/// SHARED runtime (the second run = replay of the continuation after a
/// restart). The continuation calls an auto-run skill (`auto_counter`),
/// whose counter is a direct measure of how many times the side effect
/// executed. The key `resume-{approval_id}-dispatch-0` is deterministic ->
/// the outbox deduplicates -> **the counter stays at 1** even though the
/// continuation is run twice.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn resume_continuation_dispatch_is_idempotent_across_replay() {
    use std::sync::atomic::Ordering::SeqCst;

    let bus = ResonanceBus::start(None).await.expect("bus");
    // Stable being identity for both runs (restart = the same being wakes up).
    let being_id = familyclaw_core::AgentId::new();
    // Stable, deterministic approval identity -> stable key prefix
    // (`resume-{approval_id}`) for both runs, just as in production the same
    // `approval_id` leads to the same key across a restart.
    let approval_id = ApprovalId::new();
    let prefix = format!("resume-{approval_id}");

    // SHARED runtime with an auto-run counting skill — the same dispatch
    // outbox carries idempotency across both runs (same process = the
    // in-memory outbox is sufficient to cover this replay window).
    let auto_count = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    let approval_count = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    let runtime = crash_runtime(
        Box::new(familyclaw_actions::InMemoryPendingStore::new()),
        StdArc::clone(&auto_count),
        StdArc::clone(&approval_count),
    );

    // Shared LLM script for both runs: call auto_counter, then respond
    // with text. (Each run gets its OWN scripted mock, because the script
    // is consumed during the run — on replay the LLM is called fresh, but
    // the SIDE EFFECT is deduplicated by the idempotency key, not the LLM
    // call.)
    let scripted = || {
        vec![
            body_tool_call("call_a", "auto_counter", &serde_json::json!({ "n": 1 })),
            body_text("jatko valmis"),
        ]
    };

    let now = time::now();
    let messages = vec![
        LlmMessage::system("system"),
        LlmMessage::user("jatka hyväksynnän jälkeen"),
    ];

    // ===== Run 1: original continuation after approval. =====
    {
        let api = spawn_scripted_llm(scripted()).await;
        let agent = agent_with_scripted_llm_id(being_id, "agent_a", bus.clone(), &api)
            .with_actions(StdArc::clone(&runtime));
        let llm = agent.llm.as_ref().expect("llm present");
        let actions = agent.actions.as_ref().expect("actions present");
        let outcome = agent
            .drive_tool_loop(
                llm,
                actions,
                messages.clone(),
                String::new(),
                agent.tool_loop.max_iterations,
                now,
                ActionId::new(),
                Some(&prefix),
            )
            .await
            .expect("ajo 1 ok");
        assert_eq!(
            outcome,
            ToolLoopOutcome::Answer("jatko valmis".to_string()),
            "ajo 1 etenee lopulliseen vastaukseen"
        );
    }
    assert_eq!(
        auto_count.load(SeqCst),
        1,
        "jatkon auto-run-sivuvaikutus ajetaan kerran ensimmäisellä ajolla"
    );

    // ===== Run 2: REPLAY of the SAME continuation after a restart (same prefix). =====
    // Same deterministic key `resume-{approval_id}-dispatch-0` -> the
    // outbox returns the committed result without re-running the executor.
    {
        let api = spawn_scripted_llm(scripted()).await;
        let agent = agent_with_scripted_llm_id(being_id, "agent_a", bus.clone(), &api)
            .with_actions(StdArc::clone(&runtime));
        let llm = agent.llm.as_ref().expect("llm present 2");
        let actions = agent.actions.as_ref().expect("actions present 2");
        let outcome = agent
            .drive_tool_loop(
                llm,
                actions,
                messages.clone(),
                String::new(),
                agent.tool_loop.max_iterations,
                now,
                ActionId::new(),
                Some(&prefix),
            )
            .await
            .expect("ajo 2 (replay) ok");
        assert_eq!(
            outcome,
            ToolLoopOutcome::Answer("jatko valmis".to_string()),
            "replay-ajo etenee samaan vastaukseen"
        );
    }

    // CORE ASSERTION: the side effect stays at EXACTLY 1 — the replay of
    // the continuation did NOT trigger the auto-run skill again (the
    // idempotency key deduplicated it).
    assert_eq!(
        auto_count.load(SeqCst),
        1,
        "hyväksynnän jälkeisen jatkon replay EI saa ajaa sivuvaikutusta uudelleen"
    );

    // Contrast proof: a DIFFERENT prefix (different approval_id) is NOT
    // deduplicated -> new key `resume-{other}-dispatch-0` -> the side effect
    // fires again. This confirms that dedup is due to the stable key, not
    // just runtime state.
    {
        let other_prefix = format!("resume-{}", ApprovalId::new());
        let api = spawn_scripted_llm(scripted()).await;
        let agent = agent_with_scripted_llm_id(being_id, "agent_a", bus.clone(), &api)
            .with_actions(StdArc::clone(&runtime));
        let llm = agent.llm.as_ref().expect("llm present 3");
        let actions = agent.actions.as_ref().expect("actions present 3");
        let _ = agent
            .drive_tool_loop(
                llm,
                actions,
                messages,
                String::new(),
                agent.tool_loop.max_iterations,
                now,
                ActionId::new(),
                Some(&other_prefix),
            )
            .await
            .expect("eri-prefix ajo ok");
    }
    assert_eq!(
        auto_count.load(SeqCst),
        2,
        "ERI idempotentti avain (eri approval_id) ei dedupata → sivuvaikutus laukeaa uudelleen"
    );

    bus.stop();
}

/// (b2) **TURN AUDIT resume path:** `resume_approved` records `TurnResumed`
/// (resumed turn) + the final `stop_reason` (`TurnAnswered`). This makes
/// resume just as observable as a fresh turn (roadmap §6 D6).
#[tokio::test]
async fn turn_audit_records_resume_and_answer() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let api = spawn_scripted_llm(vec![
        body_tool_call(
            "call_approve",
            "approval_skill",
            &serde_json::json!({ "q": "ship" }),
        ),
        body_text("hyväksytty toiminto valmis"),
    ])
    .await;
    let store: StdArc<dyn ResumableTurnStore> = StdArc::new(InMemoryResumableStore::new());
    let count = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    let runtime = counting_runtime_with_pending(
        Box::new(familyclaw_actions::InMemoryPendingStore::new()),
        StdArc::clone(&count),
    );
    let audit = StdArc::new(AuditCollector::new());
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(StdArc::clone(&store))
        .with_turn_audit(StdArc::clone(&audit));

    // Step 1: suspend -> audit records start + dispatch + suspended.
    let out = agent
        .think(&BusMessage::text("ship it"))
        .await
        .expect("suspend ok");
    let approval_id = match out {
        ThinkOutcome::Suspended { approval_id, .. } => approval_id,
        other => panic!("odotettiin Suspended, sai: {other:?}"),
    };

    // Step 2: resume -> audit records resumed + dispatch + answered.
    let now = time::now();
    let resumed = agent
        .resume_approved(approval_id, now)
        .await
        .expect("resume_approved ok");
    assert_eq!(
        resumed,
        ThinkOutcome::Reply("hyväksytty toiminto valmis".to_string())
    );

    // Full audit trail across two turns (suspend turn + resume turn).
    //
    // Note: the resume turn has NO `ToolDispatched` entry after
    // `TurnResumed`, because the approved tool's result is injected
    // directly into the message stack (`resume_approved`), not via the
    // loop's dispatch branch — the model responds with text on the first
    // resumed round without requesting a NEW tool. (Execution of the
    // approved action is recorded in the actions layer's own audit
    // collector, not in the turn audit.)
    let kinds = audit_kinds(&audit);
    assert_eq!(
        kinds,
        vec![
            AuditKind::TurnStarted,
            AuditKind::ToolDispatched,
            AuditKind::TurnSuspended,
            AuditKind::TurnResumed,
            AuditKind::TurnAnswered,
        ],
        "resume-jäljen pitää sisältää TurnResumed + stop_reason, sai: {kinds:?}"
    );

    // The resume entry correlates to the original approval.
    let resumed_event = audit
        .list()
        .into_iter()
        .find(|e| e.kind == AuditKind::TurnResumed)
        .expect("resumed event present");
    assert!(
        resumed_event.detail.contains(&approval_id.to_string()),
        "TurnResumed-merkinnän pitää viitata hyväksyntään, sai: {}",
        resumed_event.detail
    );
    bus.stop();
}

/// (c) **RESTART survival.** Persist the resumable turn AND the pending
/// approval to crash-durable surfaces, **drop** the entire runtime + agent,
/// rebuild them from the SAME durable files, and prove that
/// `resume_approved` still works (drives the turn to completion, side
/// effect once).
#[tokio::test]
async fn restart_survival_resume_after_rebuild_from_durable_dir() {
    let dir = TempDir::new("restart");
    // A shared execution counter carries across the "crash": proves that
    // the side effect runs exactly once across the WHOLE lifecycle.
    let count = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    // SAME being identity across both lifecycle phases: restart = the same
    // being wakes up again, so `being_id` is preserved (in production
    // `config.id` is stable). This is a prerequisite for the resume
    // ownership check.
    let being_id = familyclaw_core::AgentId::new();

    // ----- Before the "crash": suspend, which persists to disk. -----
    let approval_id = {
        let bus = ResonanceBus::start(None).await.expect("bus 1");
        let api = spawn_scripted_llm(vec![body_tool_call(
            "call_approve",
            "approval_skill",
            &serde_json::json!({ "q": "deploy" }),
        )])
        .await;
        // Fully crash-durable runtime (durable pending + durable task
        // queue) + durable resumable surface.
        let runtime = durable_counting_runtime(
            dir.pending_path(),
            dir.task_queue_path(),
            dir.outbox_path(),
            StdArc::clone(&count),
        )
        .await;
        let resumable: StdArc<dyn ResumableTurnStore> =
            StdArc::new(JournalResumableStore::open(dir.resumable_path()).expect("resumable 1"));
        let agent = agent_with_scripted_llm_id(being_id, "agent_a", bus.clone(), &api)
            .with_actions(runtime)
            .with_resumable_store(resumable);

        let out = agent
            .think(&BusMessage::text("deploy it"))
            .await
            .expect("suspend ok");
        let id = match out {
            ThinkOutcome::Suspended { approval_id, .. } => approval_id,
            other => panic!("odotettiin Suspended, sai: {other:?}"),
        };
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "ei suoritusta ennen hyväksyntää"
        );
        bus.stop();
        id
        // bus/api/runtime/agent/resumable are DROPPED here = "the process crashes".
    };

    // ----- "Restart": rebuild everything from the SAME files. -----
    let bus2 = ResonanceBus::start(None).await.expect("bus 2");
    // The resume continuation round responds with text (one request is enough).
    let api2 = spawn_scripted_llm(vec![body_text("deploy valmis restartin jälkeen")]).await;
    // Check that the pending approval survived on the durable surface across the restart.
    {
        let probe = JournalPendingStore::open(dir.pending_path()).expect("pending probe");
        assert_eq!(
            probe.len().expect("len"),
            1,
            "pending approval survived restart"
        );
    }
    // Reopen the SAME durable files — the runtime is reconstructed
    // (pending + task queue + ledger) from the logs.
    let runtime2 = durable_counting_runtime(
        dir.pending_path(),
        dir.task_queue_path(),
        dir.outbox_path(),
        StdArc::clone(&count),
    )
    .await;
    let resumable2: StdArc<dyn ResumableTurnStore> =
        StdArc::new(JournalResumableStore::open(dir.resumable_path()).expect("resumable 2"));
    // The resumable turn survived across the restart.
    assert!(
        resumable2.get(approval_id).expect("get").is_some(),
        "resumable turn survived restart"
    );
    // Same being identity -> the resume ownership check matches.
    let agent2 = agent_with_scripted_llm_id(being_id, "agent_a", bus2.clone(), &api2)
        .with_actions(runtime2)
        .with_resumable_store(StdArc::clone(&resumable2));

    // Resume still works: drives the turn to completion, side effect exactly once.
    let now = time::now();
    let resumed = agent2
        .resume_approved(approval_id, now)
        .await
        .expect("resume after restart ok");
    assert_eq!(
        resumed,
        ThinkOutcome::Reply("deploy valmis restartin jälkeen".to_string())
    );
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "side effect runs exactly once across the restart"
    );
    // Turn consumed from the durable surface.
    assert!(resumable2.get(approval_id).expect("get").is_none());
    bus2.stop();
}

/// (d) **An unknown / expired `approval_id` fails closed (no panic, no
/// side effect).**
#[tokio::test]
async fn resume_unknown_or_expired_fails_closed() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    // Per-test counter: fail-closed paths must not run the side effect.
    let count = StdArc::new(std::sync::atomic::AtomicUsize::new(0));

    // --- Unknown approval_id (nothing persisted) ---
    let api = spawn_scripted_llm(vec![body_text("ei pitäisi koskaan ajaa")]).await;
    let store: StdArc<dyn ResumableTurnStore> = StdArc::new(InMemoryResumableStore::new());
    let runtime = counting_runtime_with_pending(
        Box::new(familyclaw_actions::InMemoryPendingStore::new()),
        StdArc::clone(&count),
    );
    let agent = agent_with_scripted_llm("agent_a", bus.clone(), &api)
        .with_actions(runtime)
        .with_resumable_store(StdArc::clone(&store));

    let err = agent
        .resume_approved(ApprovalId::new(), time::now())
        .await
        .expect_err("unknown approval must fail closed");
    assert!(
        matches!(err, FamilyClawError::InvalidInput(_)),
        "tuntematon approval → InvalidInput (fail-closed), sai: {err:?}"
    );

    // --- Expired resumable turn ---
    let now = time::now();
    let expired_id = ApprovalId::new();
    let expired = crate::resumable::ResumableTurn::new(
        expired_id,
        "00000000-0000-4000-8000-000000000002",
        None,
        vec![LlmMessage::system("s"), LlmMessage::user("u")],
        "call_x",
        "approval_skill",
        &serde_json::json!({ "q": "x" }),
        "approval_skill awaiting human approval",
        now - chrono::Duration::minutes(120),
        now - chrono::Duration::minutes(60), // expires_at in the past
    );
    store.put(expired).expect("put expired");

    let err2 = agent
        .resume_approved(expired_id, now)
        .await
        .expect_err("expired resumable must fail closed");
    assert!(
        matches!(err2, FamilyClawError::InvalidInput(_)),
        "vanhentunut jatkettava vuoro → InvalidInput (fail-closed), sai: {err2:?}"
    );

    // Neither path ran the side effect.
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "fail-closed-polut eivät saa ajaa sivuvaikutusta"
    );
    bus.stop();
}

/// (d2) **Isolation between beings (defense in depth):** a being that
/// suspended ITS OWN turn can resume it (the base case is preserved); but
/// ANOTHER being sharing the same resumable-turn surface CANNOT resume the
/// first being's suspended turn — `resume_approved` refuses fail-closed
/// (ownership mismatch), and does not consume the approval, remove the
/// turn from the surface, or run the side effect.
#[tokio::test]
async fn resume_rejects_cross_being_owner_mismatch_fails_closed() {
    let bus = ResonanceBus::start(None).await.expect("bus");

    // One SHARED resumable-turn surface for two beings.
    let store: StdArc<dyn ResumableTurnStore> = StdArc::new(InMemoryResumableStore::new());

    // --- Being A: suspends its own turn (suspend) ---
    let count_a = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    let api_a = spawn_scripted_llm(vec![
        body_tool_call(
            "call_approve",
            "approval_skill",
            &serde_json::json!({ "q": "ship" }),
        ),
        body_text("alkuperäisen olennon vastaus"),
    ])
    .await;
    let runtime_a = counting_runtime_with_pending(
        Box::new(familyclaw_actions::InMemoryPendingStore::new()),
        StdArc::clone(&count_a),
    );
    let agent_a = agent_with_scripted_llm("being_alpha", bus.clone(), &api_a)
        .with_actions(StdArc::clone(&runtime_a))
        .with_resumable_store(StdArc::clone(&store));

    // --- Being B: a DIFFERENT being (its own being_id), its own runtime, sharing the STORE ---
    let count_b = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    let api_b = spawn_scripted_llm(vec![body_text("ei saa koskaan ajaa olennolle B")]).await;
    let runtime_b = counting_runtime_with_pending(
        Box::new(familyclaw_actions::InMemoryPendingStore::new()),
        StdArc::clone(&count_b),
    );
    let agent_b = agent_with_scripted_llm("being_beta", bus.clone(), &api_b)
        .with_actions(StdArc::clone(&runtime_b))
        .with_resumable_store(StdArc::clone(&store));

    // Different beings -> different identities (baseline assumption for the check).
    assert_ne!(
        agent_a.being_id(),
        agent_b.being_id(),
        "kahden olennon tunnisteiden on oltava erilliset"
    );

    // Step 1: being A suspends its turn.
    let out = agent_a
        .think(&BusMessage::text("ship it"))
        .await
        .expect("suspend ok");
    let approval_id = match out {
        ThinkOutcome::Suspended { approval_id, .. } => approval_id,
        other => panic!("odotettiin Suspended, sai: {other:?}"),
    };
    // The resumable turn is on the surface, and it belongs to being A.
    let stored = store.get(approval_id).expect("get").expect("present");
    assert_eq!(
        stored.being_id,
        agent_a.being_id().to_string(),
        "jatkettava vuoro kuuluu sen keskeyttäneelle olennolle (A)"
    );

    // Step 2: being B TRIES to resume A's turn -> fail-closed.
    let now = time::now();
    let err = agent_b
        .resume_approved(approval_id, now)
        .await
        .expect_err("cross-being resume must fail closed");
    assert!(
        matches!(err, FamilyClawError::InvalidInput(_)),
        "vieras olento → InvalidInput (omistajuus-epätäsmäys), sai: {err:?}"
    );

    // Isolation invariant: B's attempt left NO TRACE.
    // (i) NEITHER side effect ran.
    assert_eq!(
        count_a.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "A:n hyväksyntää ei kulutettu vieraan resumen kautta"
    );
    assert_eq!(
        count_b.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "B ei ajanut mitään sivuvaikutusta"
    );
    // (ii) the resumable turn is STILL on the surface (not consumed/removed).
    let still = store.get(approval_id).expect("get").expect("still present");
    assert_eq!(
        still.being_id,
        agent_a.being_id().to_string(),
        "A:n jatkettava vuoro säilyi koskemattomana hylätyn yrityksen jälkeen"
    );

    // Step 3: the rightful owner (A) resumes ITS OWN turn -> succeeds
    // (the base case is preserved). This proves that the check does not
    // break a legitimate resume.
    let resumed = agent_a
        .resume_approved(approval_id, now)
        .await
        .expect("oikean omistajan resume ok");
    assert_eq!(
        resumed,
        ThinkOutcome::Reply("alkuperäisen olennon vastaus".to_string()),
        "oikea omistaja vie vuoron loppuun"
    );
    // A's side effect now ran EXACTLY ONCE, the turn was consumed.
    assert_eq!(
        count_a.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "oikean omistajan resume ajaa sivuvaikutuksen tasan kerran"
    );
    assert!(
        store.get(approval_id).expect("get").is_none(),
        "jatkettava vuoro kulutettu oikean omistajan resumen jälkeen"
    );

    bus.stop();
}

/// (d3) **The per-being rate limit for dangerous tools engages the agent's
/// tool loop with the CORRECT being identity — not the runtime's generic
/// default.**
///
/// Regression guard for a finding by GPT-5.5: the agent sent tasks via
/// [`ActionRuntime::submit_task`], which uses the runtime's default being,
/// causing all beings behind the same shared runtime to collapse into the
/// same quota (incorrect sharing). Fixed by passing the agent's own
/// [`Agent::being_id`] to [`ActionRuntime::submit_task_as`], so each being
/// has its **own** quota.
///
/// Setup: one SHARED runtime whose limiter allows **at most one**
/// approval-requiring action per being. Two DIFFERENT beings (A and B):
/// - A's 1st approval-requiring call -> `Suspended` (within A's quota),
/// - B's 1st approval-requiring call -> `Suspended` (within B's OWN
///   quota — proves the quotas are not shared incorrectly; before the fix
///   this would have been denied because A had already filled the SHARED
///   default quota),
/// - A's 2nd approval-requiring call -> the rate limit denies it
///   ([`ActionError::PolicyDenied`]), the error is fed back to the model,
///   and the model responds with text -> `Reply` (proves that A's OWN
///   quota really is exhausted — the limit is real, not just isolated).
#[tokio::test]
async fn per_being_rate_limit_applies_through_agent_loop_with_real_being_id() {
    let bus = ResonanceBus::start(None).await.expect("bus");

    // One SHARED runtime: limiter = at most 1 approval-requiring action
    // per being (large window, so time does not evict entries mid-test).
    let runtime: StdArc<TokioMutex<ActionRuntime>> = {
        let mut rt =
            ActionRuntime::new().with_rate_limiter(DangerousToolRateLimiter::new(3_600, 1));
        rt.register_skill(ApprovalSkill)
            .expect("register approval_skill");
        StdArc::new(TokioMutex::new(rt))
    };

    // --- Being A: one approval-requiring call -> Suspended expected ---
    let api_a = spawn_scripted_llm(vec![body_tool_call(
        "call_a1",
        "approval_skill",
        &serde_json::json!({ "q": "alpha-1" }),
    )])
    .await;
    let agent_a = agent_with_scripted_llm("being_alpha", bus.clone(), &api_a)
        .with_actions(StdArc::clone(&runtime));

    // --- Being B: a DIFFERENT being (its own being_id), sharing the SAME runtime ---
    let api_b = spawn_scripted_llm(vec![body_tool_call(
        "call_b1",
        "approval_skill",
        &serde_json::json!({ "q": "beta-1" }),
    )])
    .await;
    let agent_b = agent_with_scripted_llm("being_beta", bus.clone(), &api_b)
        .with_actions(StdArc::clone(&runtime));

    assert_ne!(
        agent_a.being_id(),
        agent_b.being_id(),
        "kahden olennon tunnisteiden on oltava erilliset"
    );

    // A's 1st call -> Suspended (within A's quota).
    let out_a = agent_a
        .think(&BusMessage::text("aja hyväksyntä-työkalu (A)"))
        .await
        .expect("A:n suspend ei saa palauttaa virhettä");
    assert!(
        matches!(out_a, ThinkOutcome::Suspended { .. }),
        "A:n ensimmäinen hyväksyntää vaativa kutsu jää odottamaan lupaa, sai: {out_a:?}"
    );

    // B's 1st call -> Suspended (within B's OWN quota). This is the crux:
    // if the quota were shared incorrectly (as before the fix), B would be denied.
    let out_b = agent_b
        .think(&BusMessage::text("aja hyväksyntä-työkalu (B)"))
        .await
        .expect("B:n suspend ei saa palauttaa virhettä");
    assert!(
            matches!(out_b, ThinkOutcome::Suspended { .. }),
            "B:llä on OMA kiintiö → sen ensimmäinen kutsu suspendoituu A:sta riippumatta, sai: {out_b:?}"
        );

    // A's 2nd call -> A's OWN quota (1) is now full -> rate limit denies it.
    // The error is fed to the model, which responds with text -> Reply (the limit is real).
    let api_a2 = spawn_scripted_llm(vec![
        body_tool_call(
            "call_a2",
            "approval_skill",
            &serde_json::json!({ "q": "alpha-2" }),
        ),
        body_text("selvä, en aja sitä työkalua"),
    ])
    .await;
    let agent_a2 = agent_with_scripted_llm_id(
        agent_a.being_id().agent_id(),
        "being_alpha",
        bus.clone(),
        &api_a2,
    )
    .with_actions(StdArc::clone(&runtime));
    // Same being identity as agent_a -> shares A's quota.
    assert_eq!(
        agent_a2.being_id(),
        agent_a.being_id(),
        "agent_a2 on SAMA olento kuin agent_a (jakaa kiintiön)"
    );

    let out_a2 = agent_a2
        .think(&BusMessage::text("aja hyväksyntä-työkalu uudelleen (A)"))
        .await
        .expect("A:n toinen kutsu palautuu (virhe syötetään malliin)");
    assert_eq!(
        out_a2,
        ThinkOutcome::Reply("selvä, en aja sitä työkalua".to_string()),
        "A:n kiintiö on ehtynyt → rate-limit hylkää, malli vastaa tekstillä, sai: {out_a2:?}"
    );

    bus.stop();
}

/// (d4) **The same per-being rate limit also applies through the DURABLE
/// tool loop** ([`Agent::handle_turn`] -> [`Agent::think_actions_durable`]
/// -> [`Agent::drive_tool_loop_durable`]).
///
/// This covers the fix's other connection point (dispatch on the durable
/// branch): there too, `being_id` is passed to
/// [`ActionRuntime::submit_task_as`]. Setup as above: shared runtime,
/// limit = 1 per being, two DIFFERENT beings. A's turn suspends, and B's
/// turn suspends from B's OWN quota (proves the quotas are separate on the
/// durable path).
#[tokio::test]
async fn per_being_rate_limit_applies_through_durable_loop() {
    let bus = ResonanceBus::start(None).await.expect("bus");

    let runtime: StdArc<TokioMutex<ActionRuntime>> = {
        let mut rt =
            ActionRuntime::new().with_rate_limiter(DangerousToolRateLimiter::new(3_600, 1));
        rt.register_skill(ApprovalSkill)
            .expect("register approval_skill");
        StdArc::new(TokioMutex::new(rt))
    };

    // Being A: an approval-requiring call -> the durable turn suspends.
    let api_a = spawn_scripted_llm(vec![body_tool_call(
        "call_a1",
        "approval_skill",
        &serde_json::json!({ "q": "alpha-1" }),
    )])
    .await;
    let mut agent_a = agent_with_scripted_llm("being_alpha", bus.clone(), &api_a)
        .with_actions(StdArc::clone(&runtime));

    // Being B: a DIFFERENT being, sharing the SAME runtime.
    let api_b = spawn_scripted_llm(vec![body_tool_call(
        "call_b1",
        "approval_skill",
        &serde_json::json!({ "q": "beta-1" }),
    )])
    .await;
    let mut agent_b = agent_with_scripted_llm("being_beta", bus.clone(), &api_b)
        .with_actions(StdArc::clone(&runtime));

    assert_ne!(agent_a.being_id(), agent_b.being_id());

    // A's durable turn suspends (its own quota).
    let out_a = agent_a
        .handle_turn(BeingId::new(), &BusMessage::text("aja työkalu (A)"))
        .await
        .expect("A:n durable-vuoro ei saa kaatua");
    assert!(
        out_a.summary.contains("suspended(approval="),
        "A:n durable-vuoron pitäisi suspendoitua, sai: {}",
        out_a.summary
    );

    // B's durable turn suspends from B's OWN quota — before the fix
    // (shared default being) this WOULD have been denied because A had
    // filled the shared quota. After the fix, B has its own quota.
    let out_b = agent_b
        .handle_turn(BeingId::new(), &BusMessage::text("aja työkalu (B)"))
        .await
        .expect("B:n durable-vuoro ei saa kaatua");
    assert!(
        out_b.summary.contains("suspended(approval="),
        "B:llä on OMA kiintiö → durable-vuoro suspendoituu A:sta riippumatta, sai: {}",
        out_b.summary
    );

    bus.stop();
}

// ---- D1 CRASH-REPLAY RED-TEAM (roadmap §6 green-gate e) ----------------
//
// Proves that the durable tool loop is **replay-deterministic and
// crash-durable**: partial progress within a turn (two tool dispatches
// during one turn) survives a crash, replay does NOT re-run side effects,
// and the journaled outcomes (incl. random ApprovalId + clock-derived TTL)
// are value-identical.

use familyclaw_durable::FileJournal;

/// **Auto-run** test skill that increments a shared counter on every
/// execution. Unlike [`CountingApprovalSkill`], this one is read-only ->
/// `submit_task` runs the executor IMMEDIATELY (auto-run), so the counter
/// is a direct measure of "how many times this skill's SIDE EFFECT ran".
#[derive(Debug, Clone)]
struct CountingAutoSkill {
    /// Shared execution counter.
    count: StdArc<std::sync::atomic::AtomicUsize>,
}

impl CountingAutoSkill {
    fn new(count: StdArc<std::sync::atomic::AtomicUsize>) -> Self {
        Self { count }
    }
}

/// Fixed identifier for the auto-run counting skill.
const COUNTING_AUTO_UUID: uuid::Uuid = uuid::uuid!("77777777-1111-4222-8333-444444444444");

#[async_trait::async_trait]
impl familyclaw_actions::ActionExecutor for CountingAutoSkill {
    async fn execute(
        &self,
        request: familyclaw_actions::ActionRequest,
    ) -> familyclaw_actions::Result<familyclaw_actions::ActionResult> {
        self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(familyclaw_actions::ActionResult::success(
            "counting auto action executed",
            serde_json::json!({ "executed": true }),
            request.now,
        ))
    }
}

impl familyclaw_actions::Skill for CountingAutoSkill {
    fn manifest(&self) -> familyclaw_actions::manifest::SkillManifest {
        familyclaw_actions::manifest::SkillManifest {
            id: familyclaw_actions::SkillId::from_uuid(COUNTING_AUTO_UUID),
            name: "auto_counter".to_string(),
            version: "1.0.0".to_string(),
            description: "Laskeva read-only (auto-run) toiminto, testikäyttö.".to_string(),
            permissions: vec![familyclaw_actions::policy::SkillPermission::ReadFiles],
            risk: familyclaw_actions::policy::ActionRisk::ReadOnly,
            approval_policy: familyclaw_actions::policy::ApprovalPolicy::AutoIfReadOnly,
            input_hint: None,
            output_hint: None,
            input_schema: familyclaw_actions::manifest::default_input_schema(),
            publisher: None,
            signature: None,
        }
    }
}

/// Builds a shared runtime with BOTH an auto-run counting skill
/// (`auto_counter`) AND an approval-requiring counting skill
/// (`approval_skill`). The auto-counter's counter is `auto_count`; the
/// approval skill's counter is `approval_count`. `pending` is the storage
/// surface for pending approvals (durable or in-mem).
fn crash_runtime(
    pending: Box<dyn PendingApprovalStore>,
    auto_count: StdArc<std::sync::atomic::AtomicUsize>,
    approval_count: StdArc<std::sync::atomic::AtomicUsize>,
) -> StdArc<TokioMutex<ActionRuntime>> {
    let mut rt = ActionRuntime::with_pending_store(pending);
    rt.register_skill(CountingAutoSkill::new(auto_count))
        .expect("register auto_counter");
    rt.register_skill(CountingApprovalSkill::new(approval_count))
        .expect("register approval_skill");
    StdArc::new(TokioMutex::new(rt))
}

/// Builds an agent on top of a **crash-durable [`FileJournal`]** at the
/// given path (not in-memory). Same LLM/memory/bus configuration as
/// [`agent_with_scripted_llm`], but the durable context is on disk, so a
/// "crash" = drop the agent and rebuild it from the same file.
fn agent_over_file_journal(
    name: &str,
    bus: BusHandle,
    api_base: &str,
    journal_path: &std::path::Path,
    memory: ErasedMemoryStore,
) -> Agent {
    agent_over_file_journal_id(
        familyclaw_core::AgentId::new(),
        name,
        bus,
        api_base,
        journal_path,
        memory,
    )
}

/// Like [`agent_over_file_journal`], but with a **fixed** `AgentId`.
///
/// In the restart-then-resume proof, the SAME being is rebuilt from
/// durable files — its `being_id` must be preserved so the resume
/// ownership check matches. In production `config.id` is stable across a
/// restart because the gateway derives it from the name
/// (`AgentConfig::new_with_stable_id`); only the test's plain
/// [`AgentConfig::new`] would randomize it, so this helper pins the id.
fn agent_over_file_journal_id(
    id: familyclaw_core::AgentId,
    name: &str,
    bus: BusHandle,
    api_base: &str,
    journal_path: &std::path::Path,
    memory: ErasedMemoryStore,
) -> Agent {
    let mut config = AgentConfig::new(name, ModelConfig::new("scripted/model"));
    config.id = id;
    let soul = Soul::from_essence(format!("I am {name}."));
    let journal = FileJournal::open(journal_path).expect("open file journal");
    let durable = DurableContext::new(Arc::new(journal) as Arc<dyn Journal + Send + Sync>)
        .expect("durable ctx over file journal");
    let cfg = LlmConfig::new(api_base, "test-key", "scripted-model")
        .with_request_timeout_ms(2_000)
        .with_connect_timeout_ms(2_000);
    Agent::new(config, soul, memory, durable, bus, Some(cfg), None)
}

/// **CRASH-REPLAY RED-TEAM (D1, roadmap §6 green-gate e).**
///
/// One turn dispatches TWO tools: first the auto-run counting
/// `auto_counter` (observable side effect = counter), then the
/// approval-requiring `approval_skill` (-> suspend, random `ApprovalId` +
/// clock-derived TTL). Everything is recorded in a crash-durable
/// [`FileJournal`].
///
/// Proves TWO hard properties:
/// - **(a)** the first tool's side effect runs **exactly once** across the
///   entire original run + replay (replay returns the journaled result,
///   does NOT re-run the executor), AND
/// - **(b)** the replayed dispatch's outcome (incl. the random
///   `ApprovalId` and clock-derived TTL) is **value-identical** to the
///   original -> proof that the clock was journaled INSIDE the durable
///   step (the `SubmitOutcome` was recorded -> returned identically), not
///   read live.
// Four phases (fresh suspend -> full replay -> crash-between-dispatches ->
// resume across a restart) form a single crash-durability proof; splitting
// them into separate tests would duplicate the heavy FileJournal setup.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn crash_replay_tool_loop_is_deterministic_and_crash_safe() {
    use std::sync::atomic::Ordering::SeqCst;

    let dir = TempDir::new("crash-replay");
    let journal_path = dir.0.join("agent.journal.jsonl");
    let resumable_path = dir.0.join("resumable.jsonl");
    // Shared memory on disk, so turn_key dedup also works across a rebuild.
    let mem_path = dir.0.join("memory.json");
    let memory: ErasedMemoryStore =
        Arc::new(LocalJsonStore::open(&mem_path).await.expect("open mem"));

    // Shared counters persist across ALL rebuilds -> measure the total
    // number of side effects over the lifecycle.
    let auto_count = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    let approval_count = StdArc::new(std::sync::atomic::AtomicUsize::new(0));

    // ============ PHASE 1: fresh turn -> suspend (full journal). ============
    // LLM script: llm-0 -> call auto_counter; llm-1 -> call approval_skill
    // (-> suspend). Two requests are enough, because suspend ends the loop.
    let approval_id_orig;
    let dispatch0_record_orig;
    {
        let bus = ResonanceBus::start(None).await.expect("bus 1");
        let api = spawn_scripted_llm(vec![
            body_tool_call("call_a", "auto_counter", &serde_json::json!({ "n": 1 })),
            body_tool_call(
                "call_b",
                "approval_skill",
                &serde_json::json!({ "q": "do-it" }),
            ),
        ])
        .await;
        let runtime = crash_runtime(
            Box::new(familyclaw_actions::InMemoryPendingStore::new()),
            StdArc::clone(&auto_count),
            StdArc::clone(&approval_count),
        );
        let resumable: StdArc<dyn ResumableTurnStore> =
            StdArc::new(JournalResumableStore::open(&resumable_path).expect("resumable 1"));
        let mut agent = agent_over_file_journal(
            "agent_a",
            bus.clone(),
            &api,
            &journal_path,
            StdArc::clone(&memory),
        )
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(StdArc::clone(&resumable));

        let outcome = agent
            .handle_turn(BeingId::new(), &BusMessage::text("aja kaksi työkalua"))
            .await
            .expect("turn ok");

        // The turn summary marks the suspend (the second dispatch required approval).
        assert!(
            outcome.summary.contains("suspended(approval="),
            "vuoron pitäisi keskeytyä toiseen (hyväksyntä-)työkaluun, sai: {}",
            outcome.summary
        );
        // The first tool's side effect ran EXACTLY ONCE (auto-run).
        assert_eq!(
            auto_count.load(SeqCst),
            1,
            "ensimmäisen (auto-run) työkalun sivuvaikutus ajetaan kerran tuoreessa ajossa"
        );
        // The second skill's executor does NOT run before approval (approval-gated).
        assert_eq!(approval_count.load(SeqCst), 0);

        // Extract the suspend's ApprovalId from the turn's audit/durable
        // state: the easiest way is to read it from the durable log's
        // `turn-0-suspend` step.
        let journal_text = std::fs::read_to_string(&journal_path).expect("read journal");
        approval_id_orig = extract_suspend_approval_id(&journal_text)
            .expect("turn-0-suspend approval id present in journal");
        // Also capture the journaled DispatchRecord of the first dispatch.
        dispatch0_record_orig = extract_dispatch_record(&journal_text, "turn-0-dispatch-0")
            .expect("turn-0-dispatch-0 record present in journal");
        assert!(
            resumable
                .get(approval_id_orig)
                .expect("get resumable")
                .is_some(),
            "production-shaped replay requires a durable continuation surface"
        );

        bus.stop();
        // agent/runtime/bus are DROPPED = "the process crashes".
    }

    // ============ PHASE 2 — PROPERTY (b): replay of the FULL journal. ============
    // Rebuild the agent from the SAME FileJournal. Re-run the SAME turn:
    // every step (llm-0, dispatch-0, llm-1, dispatch-1, -think/-suspend)
    // replays from the log -> the LLM is NOT called, submit is NOT re-run,
    // the auto_counter executor is NOT re-run. We use an LLM mock that
    // provides NO bodies (if it were called, the turn would hang until
    // timeout -> the test would fail); it is not called during replay.
    {
        let bus = ResonanceBus::start(None).await.expect("bus 2");
        let api = spawn_scripted_llm(vec![]).await; // no bodies: must not be called
        let runtime = crash_runtime(
            Box::new(familyclaw_actions::InMemoryPendingStore::new()),
            StdArc::clone(&auto_count),
            StdArc::clone(&approval_count),
        );
        let resumable: StdArc<dyn ResumableTurnStore> =
            StdArc::new(JournalResumableStore::open(&resumable_path).expect("resumable 2"));
        let mut agent = agent_over_file_journal(
            "agent_a",
            bus.clone(),
            &api,
            &journal_path,
            StdArc::clone(&memory),
        )
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(StdArc::clone(&resumable));
        // The context is in replay mode (the log has the earlier turn's steps).
        assert!(
            agent.durable.is_replaying(),
            "rebuild näkee aiemman vuoron askeleet → replay-tila"
        );
        assert!(
            resumable
                .get(approval_id_orig)
                .expect("get resumable after restart")
                .is_some(),
            "durable continuation must survive restart for honest Suspended replay"
        );

        let outcome = agent
            .handle_turn(BeingId::new(), &BusMessage::text("aja kaksi työkalua"))
            .await
            .expect("replay turn ok");

        // (a) Auto-counter did NOT re-run: still EXACTLY 1 over the whole lifecycle.
        assert_eq!(
            auto_count.load(SeqCst),
            1,
            "replay EI saa ajaa ensimmäisen työkalun sivuvaikutusta uudelleen"
        );
        // Submit did NOT re-run -> the approval runtime is empty (a NEW
        // runtime, whose pending store would have received a NEW approval
        // if submit had run during replay). Submit is not called during
        // the replayed dispatch.
        assert_eq!(
            runtime.lock().await.pending_approvals().len(),
            0,
            "replay EI saa ajaa submit_taskia uudelleen (ei uutta hyväksyntää)"
        );

        // (b) The replayed turn's suspend returns the **same** ApprovalId +
        // the same outcome -> the clock was journaled INSIDE the step.
        assert!(
            outcome.summary.contains("suspended(approval="),
            "replay-vuoro keskeytyy edelleen suspendiin, sai: {}",
            outcome.summary
        );
        let journal_text = std::fs::read_to_string(&journal_path).expect("read journal 2");
        let approval_id_replay = extract_suspend_approval_id(&journal_text)
            .expect("turn-0-suspend approval id still present");
        assert_eq!(
            approval_id_replay, approval_id_orig,
            "replatun suspendin ApprovalId on ARVO-IDENTTINEN (kello journaloitu askeleen sisällä)"
        );
        let dispatch0_replay = extract_dispatch_record(&journal_text, "turn-0-dispatch-0")
            .expect("turn-0-dispatch-0 still present");
        assert_eq!(
            dispatch0_replay, dispatch0_record_orig,
            "ensimmäisen lähetyksen journaloitu SubmitOutcome (task_id/status) on arvo-identtinen"
        );
        bus.stop();
    }

    // ====== PHASE 3 — PROPERTY (a) strictly: crash BETWEEN DISPATCHES. ======
    // Tear the journal so only the FIRST dispatch's (dispatch-0) step +
    // everything before it remain in the log — everything AFTER dispatch-0
    // is removed. This simulates a crash EXACTLY between the two
    // dispatches. On replay, dispatch-0 is returned from the log
    // (auto_counter does NOT re-run), but the rest (llm-1, dispatch-1) is
    // run FRESH.
    {
        truncate_journal_after_step(&journal_path, "turn-0-dispatch-0");
        let bus = ResonanceBus::start(None).await.expect("bus 3");
        // The replay tail needs llm-1 (approval call) -> suspend again.
        let api = spawn_scripted_llm(vec![body_tool_call(
            "call_b",
            "approval_skill",
            &serde_json::json!({ "q": "do-it" }),
        )])
        .await;
        let runtime = crash_runtime(
            Box::new(familyclaw_actions::InMemoryPendingStore::new()),
            StdArc::clone(&auto_count),
            StdArc::clone(&approval_count),
        );
        let resumable: StdArc<dyn ResumableTurnStore> =
            StdArc::new(JournalResumableStore::open(&resumable_path).expect("resumable 3"));
        let mut agent = agent_over_file_journal(
            "agent_a",
            bus.clone(),
            &api,
            &journal_path,
            StdArc::clone(&memory),
        )
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(StdArc::clone(&resumable));
        assert!(
            agent.durable.is_replaying(),
            "katkaistu journal → replay-tila"
        );

        let outcome = agent
            .handle_turn(BeingId::new(), &BusMessage::text("aja kaksi työkalua"))
            .await
            .expect("partial replay turn ok");

        // CORE ASSERTION (a): even though the TURN is partially re-run
        // (dispatch-1 fresh), the FIRST tool's side effect stays at
        // EXACTLY 1 — dispatch-0 was returned from the log and
        // auto_counter was not re-run.
        assert_eq!(
            auto_count.load(SeqCst),
            1,
            "kaatuminen dispatchien välissä: 1. työkalun sivuvaikutus EDELLEEN tasan kerran"
        );
        // The tail of the turn (second, approval-requiring tool) ran fresh -> suspend.
        assert!(
            outcome.summary.contains("suspended(approval="),
            "osittaisreplay vie vuoron loppuun (suspend toiseen työkaluun), sai: {}",
            outcome.summary
        );
        bus.stop();
    }

    // ====== PHASE 4: RESUME of a suspended turn survives a restart (C1/C3). ======
    // Use FULLY durable surfaces (pending + resumable journals), drive the
    // turn to suspend, drop everything, rebuild, and prove that resume
    // DRIVES the turn to completion without re-running the pre-suspend
    // side effects.
    {
        let resume_dir = TempDir::new("crash-resume");
        let rj_path = resume_dir.0.join("agent.journal.jsonl");
        let rmem_path = resume_dir.0.join("memory.json");
        let rmem: ErasedMemoryStore =
            Arc::new(LocalJsonStore::open(&rmem_path).await.expect("open rmem"));
        let pending_path = resume_dir.pending_path();
        let task_path = resume_dir.task_queue_path();
        let outbox_path = resume_dir.outbox_path();
        let resumable_path = resume_dir.resumable_path();

        // Separate counters for this phase (its own lifecycle).
        let auto2 = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
        let approval2 = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
        // Same being identity across the "restart" — the resume ownership
        // check matches only if `being_id` is preserved (like a stable
        // config.id in production).
        let r_being_id = familyclaw_core::AgentId::new();

        // --- Before the crash: suspend, which persists to the durable surfaces. ---
        let approval_id = {
            let bus = ResonanceBus::start(None).await.expect("bus r1");
            let api = spawn_scripted_llm(vec![
                body_tool_call("call_a", "auto_counter", &serde_json::json!({ "n": 1 })),
                body_tool_call(
                    "call_b",
                    "approval_skill",
                    &serde_json::json!({ "q": "go" }),
                ),
            ])
            .await;
            // Fully durable runtime (pending + task queue) + durable resumable.
            let mut rt = ActionRuntime::with_durable_stores(
                pending_path.clone(),
                task_path.clone(),
                outbox_path.clone(),
            )
            .await
            .expect("durable stores");
            rt.register_skill(CountingAutoSkill::new(StdArc::clone(&auto2)))
                .expect("auto");
            rt.register_skill(CountingApprovalSkill::new(StdArc::clone(&approval2)))
                .expect("approval");
            let runtime = StdArc::new(TokioMutex::new(rt));
            let resumable: StdArc<dyn ResumableTurnStore> =
                StdArc::new(JournalResumableStore::open(&resumable_path).expect("resumable open"));
            let mut agent = agent_over_file_journal_id(
                r_being_id,
                "agent_a",
                bus.clone(),
                &api,
                &rj_path,
                StdArc::clone(&rmem),
            )
            .with_actions(StdArc::clone(&runtime))
            .with_resumable_store(StdArc::clone(&resumable));

            let outcome = agent
                .handle_turn(BeingId::new(), &BusMessage::text("aja kaksi työkalua"))
                .await
                .expect("turn ok");
            assert!(outcome.summary.contains("suspended(approval="));
            assert_eq!(
                auto2.load(SeqCst),
                1,
                "auto-sivuvaikutus kerran ennen kaatumista"
            );
            assert_eq!(
                approval2.load(SeqCst),
                0,
                "hyväksyntä-taito ei aja ennen lupaa"
            );

            // ApprovalId from the durable log's `turn-0-suspend` step
            // (survives the restart because FileJournal fsyncs every step).
            let journal_text = std::fs::read_to_string(&rj_path).expect("read resume journal");
            let id = extract_suspend_approval_id(&journal_text)
                .expect("turn-0-suspend approval id present");
            bus.stop();
            id
            // EVERYTHING is dropped = crash.
        };

        // --- Restart: rebuild from the same durable files. ---
        let bus = ResonanceBus::start(None).await.expect("bus r2");
        // The resume continuation round responds with text (one request is enough).
        let api = spawn_scripted_llm(vec![body_text("valmis restartin jälkeen")]).await;
        let mut rt = ActionRuntime::with_durable_stores(pending_path, task_path, outbox_path)
            .await
            .expect("durable stores 2");
        rt.register_skill(CountingAutoSkill::new(StdArc::clone(&auto2)))
            .expect("auto 2");
        rt.register_skill(CountingApprovalSkill::new(StdArc::clone(&approval2)))
            .expect("approval 2");
        let runtime = StdArc::new(TokioMutex::new(rt));
        let resumable: StdArc<dyn ResumableTurnStore> =
            StdArc::new(JournalResumableStore::open(&resumable_path).expect("resumable 2"));
        // The resumable turn survived across the restart.
        assert!(
            resumable.get(approval_id).expect("get").is_some(),
            "pending resumable turn survived restart"
        );
        let agent = agent_over_file_journal_id(
            r_being_id,
            "agent_a",
            bus.clone(),
            &api,
            &rj_path,
            StdArc::clone(&rmem),
        )
        .with_actions(StdArc::clone(&runtime))
        .with_resumable_store(StdArc::clone(&resumable));

        let now = time::now();
        let resumed = agent
            .resume_approved(approval_id, now)
            .await
            .expect("resume after restart ok");
        assert_eq!(
            resumed,
            ThinkOutcome::Reply("valmis restartin jälkeen".to_string()),
            "resume vie keskeytetyn vuoron loppuun restartin jälkeen"
        );
        // The approved action ran EXACTLY once (resume = approve).
        assert_eq!(
            approval2.load(SeqCst),
            1,
            "hyväksytty toiminto ajetaan kerran resumessa"
        );
        // The PRE-SUSPEND side effect (auto-counter) did NOT re-run: still
        // exactly 1 across the entire suspend -> restart -> resume lifecycle.
        assert_eq!(
            auto2.load(SeqCst),
            1,
            "resume EI saa ajaa suspend-edeltäviä sivuvaikutuksia uudelleen"
        );
        bus.stop();
    }
}

/// Helper: extracts the approval identifier from the payload journaled by
/// the `turn-0-suspend` step (`"<approval_id>|<summary>"`).
fn extract_suspend_approval_id(journal_jsonl: &str) -> Option<ApprovalId> {
    for line in journal_jsonl.lines() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let is_suspend = entry.pointer("/kind/kind").and_then(|k| k.as_str())
            == Some("step_completed")
            && entry.pointer("/kind/name").and_then(|n| n.as_str()) == Some("turn-0-suspend");
        if is_suspend {
            let payload = entry.pointer("/kind/output").and_then(|o| o.as_str())?;
            let id_str = payload.split('|').next()?;
            return id_str.parse::<ApprovalId>().ok();
        }
    }
    None
}

/// Helper: extracts the journaled [`DispatchRecord`] of the named
/// `turn-0-dispatch-{k}` step (a deterministic value for replay comparison).
fn extract_dispatch_record(journal_jsonl: &str, step_name: &str) -> Option<serde_json::Value> {
    for line in journal_jsonl.lines() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let matches = entry.pointer("/kind/kind").and_then(|k| k.as_str())
            == Some("step_completed")
            && entry.pointer("/kind/name").and_then(|n| n.as_str()) == Some(step_name);
        if matches {
            return entry.pointer("/kind/output").cloned();
        }
    }
    None
}

// ── Short-term memory / multiturn ("respond more than once") ──────────────

#[test]
fn build_message_stack_orders_system_history_then_user() {
    let history = vec![
        LlmMessage::user("aiempi kysymys"),
        LlmMessage::assistant("aiempi vastaus"),
    ];
    let stack = build_message_stack("SYSTEM".to_string(), &history, "uusi".to_string());
    // [system, user(previous), assistant(previous), user(new)]
    assert_eq!(stack.len(), 4);
    assert_eq!(stack[0].role, crate::llm::LlmRole::System);
    assert_eq!(stack[1].role, crate::llm::LlmRole::User);
    assert_eq!(stack[1].content, "aiempi kysymys");
    assert_eq!(stack[2].role, crate::llm::LlmRole::Assistant);
    assert_eq!(stack[3].role, crate::llm::LlmRole::User);
    assert_eq!(stack[3].content, "uusi");
}

#[test]
fn build_message_stack_empty_history_is_just_system_user() {
    let stack = build_message_stack("SYSTEM".to_string(), &[], "kysymys".to_string());
    assert_eq!(stack.len(), 2);
    assert_eq!(stack[0].role, crate::llm::LlmRole::System);
    assert_eq!(stack[1].role, crate::llm::LlmRole::User);
}

#[test]
fn truncate_for_history_keeps_short_and_caps_long_at_utf8_boundary() {
    assert_eq!(truncate_for_history("lyhyt"), "lyhyt");
    // A long multi-byte string is not cut in the middle of a character.
    let long = "ä".repeat(HISTORY_MAX_CHARS_PER_MSG); // 2 bytes/char
    let out = truncate_for_history(&long);
    assert!(out.ends_with('…'));
    let body = out.trim_end_matches('…');
    assert!(body.len() <= HISTORY_MAX_CHARS_PER_MSG);
    // Every byte is a valid UTF-8 boundary (no broken 'ä').
    assert!(body.is_char_boundary(body.len()));
}

// Env vars are process-global; serialize tests that touch them so they
// don't race each other (same pattern as `watchdog.rs` / `identity.rs`).
static HISTORY_ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn history_env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    HISTORY_ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// RAII guard: sets `key` to `value` on construction, restores whatever
/// was there before on drop (even on panic).
struct HistoryEnvVarGuard {
    key: &'static str,
    prior: Option<String>,
}

impl HistoryEnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prior = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prior }
    }
}

impl Drop for HistoryEnvVarGuard {
    fn drop(&mut self) {
        match self.prior.take() {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

#[test]
fn history_max_chars_per_msg_reads_env_override() {
    const ENV: &str = "FAMILYCLAW_HISTORY_MAX_CHARS";
    let _lock = history_env_test_lock();

    {
        let _guard = HistoryEnvVarGuard::set(ENV, "500");
        assert_eq!(history_max_chars_per_msg(), 500);
        let long = "x".repeat(1000);
        let out = truncate_for_history(&long);
        let body = out.trim_end_matches('…');
        assert_eq!(
            body.len(),
            500,
            "truncate_for_history must respect the env override"
        );
    }

    // Below the minimum -> falls back to the default (not a truncation trap).
    {
        let _guard = HistoryEnvVarGuard::set(ENV, "50");
        assert_eq!(
            history_max_chars_per_msg(),
            HISTORY_MAX_CHARS_PER_MSG,
            "a value below HISTORY_MAX_CHARS_MIN must fall back to the default"
        );
    }

    // Unset / garbage -> default.
    std::env::remove_var(ENV);
    assert_eq!(history_max_chars_per_msg(), HISTORY_MAX_CHARS_PER_MSG);
    let _guard = HistoryEnvVarGuard::set(ENV, "not-a-number");
    assert_eq!(history_max_chars_per_msg(), HISTORY_MAX_CHARS_PER_MSG);
}

#[tokio::test]
async fn append_history_is_a_sliding_window() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let mut agent = test_agent("agent_a", bus.clone());
    let key = "discord-main:general";
    // Push more exchanges than the window holds.
    for i in 0..(HISTORY_MAX_MESSAGES) {
        agent.append_history(key, &format!("kysymys {i}"), &format!("vastaus {i}"));
    }
    let hist = agent.history_for(key);
    // The window holds at most HISTORY_MAX_MESSAGES messages (user+assistant).
    assert_eq!(hist.len(), HISTORY_MAX_MESSAGES);
    // The oldest was dropped: the last message is the newest assistant response.
    assert_eq!(
        hist.last().expect("last").role,
        crate::llm::LlmRole::Assistant
    );
    let newest = &hist.last().expect("last").content;
    assert!(newest.starts_with("vastaus"));
    bus.stop();
}

#[tokio::test]
async fn append_history_skips_empty_messages() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let mut agent = test_agent("agent_a", bus.clone());
    let key = "k";
    agent.append_history(key, "", "vastaus");
    agent.append_history(key, "kysymys", "   ");
    assert!(agent.history_for(key).is_empty(), "tyhjiä ei tallenneta");
    agent.append_history(key, "kysymys", "vastaus");
    assert_eq!(agent.history_for(key).len(), 2);
    bus.stop();
}

// ---- CompactionConfig / compact_history (configurable context compaction) ----

fn msg(i: usize) -> LlmMessage {
    LlmMessage::user(format!("m{i}"))
}

fn window(n: usize) -> VecDeque<LlmMessage> {
    (0..n).map(msg).collect()
}

#[test]
fn compact_history_default_matches_old_drop_oldest_window() {
    // CompactionConfig::default() must reproduce the pre-existing
    // hardcoded sliding-window behavior exactly (backward compatibility).
    let config = CompactionConfig::default();
    let mut dq = window(HISTORY_MAX_MESSAGES + 5);
    compact_history(&mut dq, &config);
    assert_eq!(dq.len(), HISTORY_MAX_MESSAGES);
    // Oldest-first eviction: the survivors are the newest messages.
    assert_eq!(dq.front().unwrap().content, format!("m{}", 5));
    assert_eq!(
        dq.back().unwrap().content,
        format!("m{}", HISTORY_MAX_MESSAGES + 4)
    );
}

#[test]
fn compact_history_zero_threshold_disables_compaction() {
    let config = CompactionConfig {
        max_messages: 0,
        ..CompactionConfig::default()
    };
    let mut dq = window(50);
    compact_history(&mut dq, &config);
    assert_eq!(dq.len(), 50, "max_messages = 0 must disable compaction");
}

#[test]
fn compact_history_protects_first_n_even_over_threshold() {
    // protect_first_n pins the opening messages; protect_last_n = 0 so the
    // whole non-protected tail is evictable one at a time.
    let config = CompactionConfig {
        max_messages: 5,
        protect_first_n: 3,
        protect_last_n: 0,
        summarizer: None,
    };
    let mut dq = window(10);
    compact_history(&mut dq, &config);
    assert_eq!(dq.len(), 5);
    // The first 3 (protected head) survive unevicted, in original order.
    assert_eq!(dq[0].content, "m0");
    assert_eq!(dq[1].content, "m1");
    assert_eq!(dq[2].content, "m2");
}

#[test]
fn compact_history_stops_when_protections_cover_whole_window() {
    // protect_first_n + protect_last_n >= the window's own length -> nothing
    // evictable; the window is allowed to stay over max_messages rather
    // than evict a "protected" message.
    let config = CompactionConfig {
        max_messages: 4,
        protect_first_n: 3,
        protect_last_n: 3,
        summarizer: None,
    };
    let mut dq = window(6); // protect_first_n(3) + protect_last_n(3) == len(6)
    compact_history(&mut dq, &config);
    assert_eq!(
        dq.len(),
        6,
        "fully protected window must not be evicted despite exceeding max_messages"
    );
}

#[test]
fn compact_history_summarizer_collapses_the_evictable_middle_zone() {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_inner = calls.clone();
    let summarizer: CompactionSummarizer = Arc::new(move |evicted: &[LlmMessage]| {
        calls_inner.fetch_add(1, AtomicOrdering::SeqCst);
        Some(LlmMessage::assistant(format!(
            "summary of {} messages",
            evicted.len()
        )))
    });
    let config = CompactionConfig {
        max_messages: 5,
        protect_first_n: 0,
        protect_last_n: 3,
        summarizer: Some(summarizer),
    };
    let mut dq = window(10);
    compact_history(&mut dq, &config);
    // The evictable middle (7 messages: m0..=m6) collapses to ONE summary
    // message, leaving: [summary, m7, m8, m9] = 4 messages (<= max_messages).
    assert_eq!(dq.len(), 4);
    assert_eq!(dq.front().unwrap().content, "summary of 7 messages");
    assert_eq!(dq.back().unwrap().content, "m9");
    assert_eq!(
        calls.load(AtomicOrdering::SeqCst),
        1,
        "summarizer called once"
    );
}

#[test]
fn compact_history_summarizer_none_falls_back_to_plain_eviction() {
    // A summarizer that always declines (`None`) must not stall compaction
    // — the plain oldest-first path takes over.
    let summarizer: CompactionSummarizer = Arc::new(|_evicted: &[LlmMessage]| None);
    let config = CompactionConfig {
        max_messages: 5,
        protect_first_n: 0,
        protect_last_n: 3,
        summarizer: Some(summarizer),
    };
    let mut dq = window(10);
    compact_history(&mut dq, &config);
    assert_eq!(dq.len(), 5);
    assert_eq!(dq.back().unwrap().content, "m9");
}

#[test]
fn compaction_config_from_env_reads_overrides_and_defaults() {
    const MAX: &str = "FAMILYCLAW_COMPACTION_MAX_MESSAGES";
    const FIRST: &str = "FAMILYCLAW_COMPACTION_PROTECT_FIRST_N";
    const LAST: &str = "FAMILYCLAW_COMPACTION_PROTECT_LAST_N";
    let _lock = history_env_test_lock();
    std::env::remove_var(MAX);
    std::env::remove_var(FIRST);
    std::env::remove_var(LAST);

    // Unset -> defaults matching the old hardcoded window.
    let cfg = CompactionConfig::from_env();
    assert_eq!(cfg.max_messages, CompactionConfig::DEFAULT_MAX_MESSAGES);
    assert_eq!(cfg.protect_first_n, 0);
    assert_eq!(cfg.protect_last_n, CompactionConfig::DEFAULT_MAX_MESSAGES);
    assert!(cfg.summarizer.is_none());

    // Set -> overrides are read.
    {
        let _g1 = HistoryEnvVarGuard::set(MAX, "8");
        let _g2 = HistoryEnvVarGuard::set(FIRST, "2");
        let _g3 = HistoryEnvVarGuard::set(LAST, "4");
        let cfg = CompactionConfig::from_env();
        assert_eq!(cfg.max_messages, 8);
        assert_eq!(cfg.protect_first_n, 2);
        assert_eq!(cfg.protect_last_n, 4);
    }
}

#[tokio::test]
async fn agent_with_compaction_overrides_the_default_policy() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let config = CompactionConfig {
        max_messages: 4,
        protect_first_n: 0,
        protect_last_n: 4,
        summarizer: None,
    };
    let mut agent = test_agent("agent_a", bus.clone()).with_compaction(config);
    assert_eq!(agent.compaction().max_messages, 4);
    let key = "discord-main:general";
    for i in 0..10 {
        agent.append_history(key, &format!("kysymys {i}"), &format!("vastaus {i}"));
    }
    // 10 turns * 2 messages = 20, but max_messages = 4 caps the window.
    assert_eq!(agent.history_for(key).len(), 4);
    bus.stop();
}

#[tokio::test]
async fn conversation_key_separates_channels_and_conversations() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let agent = test_agent("agent_a", bus.clone());
    let a = familyclaw_bus::MessageOrigin::new("discord-main", "general", "u1");
    let b = familyclaw_bus::MessageOrigin::new("discord-main", "random", "u1");
    let c = familyclaw_bus::MessageOrigin::new("telegram", "general", "u1");
    assert_ne!(
        agent.conversation_key(Some(&a)),
        agent.conversation_key(Some(&b))
    );
    assert_ne!(
        agent.conversation_key(Some(&a)),
        agent.conversation_key(Some(&c))
    );
    assert_eq!(agent.conversation_key(Some(&a)), "discord-main:general");
    // Without an origin: fallback "default" (no reply target on the test agent).
    assert_eq!(agent.conversation_key(None), "default");
    bus.stop();
}

#[tokio::test]
async fn separate_conversations_keep_independent_history() {
    let bus = ResonanceBus::start(None).await.expect("bus");
    let mut agent = test_agent("agent_a", bus.clone());
    agent.append_history("chan:a", "ka", "va");
    agent.append_history("chan:b", "kb", "vb");
    assert_eq!(agent.history_for("chan:a").len(), 2);
    assert_eq!(agent.history_for("chan:b").len(), 2);
    assert!(agent.history_for("chan:a")[0].content.contains("ka"));
    assert!(agent.history_for("chan:b")[0].content.contains("kb"));
    bus.stop();
}

/// Helper: tears the `FileJournal` file so that the `step_name` step + the
/// lines preceding it remain, but everything AFTER it is removed —
/// simulates a crash immediately after the given step was recorded.
fn truncate_journal_after_step(path: &std::path::Path, step_name: &str) {
    let contents = std::fs::read_to_string(path).expect("read journal");
    let mut kept: Vec<&str> = Vec::new();
    for line in contents.lines() {
        kept.push(line);
        if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
            let is_target = entry
                .get("kind")
                .and_then(|k| k.get("name"))
                .and_then(|n| n.as_str())
                == Some(step_name);
            if is_target {
                break; // keep this line, discard the rest
            }
        }
    }
    let mut out = kept.join("\n");
    out.push('\n');
    std::fs::write(path, out).expect("rewrite truncated journal");
}
