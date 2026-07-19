//! End-to-end roundtrip integration test for the `FamilyClaw` runtime.
//!
//! Proves **deterministically** the entire message chain without a real LLM
//! key or Telegram token:
//!
//! ```text
//! inbound (MockChannel::inject)
//!   └─► pump_channel_to_bus ─► Resonance Bus
//!                                  └─► Agent::handle_turn ─► Agent::think()
//!                                          (mock-LLM, OpenAI-compatible HTTP,
//!                                           returns fixed text)
//!                                              └─► route_reply ─► reply_sink
//!                                                      └─► drain ─► Channel::send
//!                                                              └─► MockChannel.outbox
//! ```
//!
//! This is the equivalent of "a generic agent sends its 1st message": one
//! inbound message produces one outbound response with the correct target,
//! and the response content is EXACTLY what the mock LLM returned (no
//! randomness, no network leaving the test).
//!
//! ## Why a mock LLM as an HTTP server
//! `Agent::think()` calls `LlmClient::complete()`, which makes an `OpenAI`-
//! compatible `POST /chat/completions` request. We spin up a tiny axum
//! server that returns a fixed choices[0].message.content value, and point
//! the `EnvEndpointResolver` provider at its base URL. This way the whole
//! reply path is actually exercised (think → `route_reply` → sink → send)
//! without an external API.

use std::net::SocketAddr;
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use familyclaw_agent::EnvEndpointResolver;
use familyclaw_channels::{InboundMessage, MockChannel, OutboundKind, OutboundMessage};
use familyclaw_core::{AgentConfig, ModelConfig};
use familyclaw_runtime::build_family;

/// Fixed text that the mock LLM always returns. The roundtrip's "proof": if
/// this text ends up in the `MockChannel.outbox` queue, the whole chain worked.
const FIXED_LLM_REPLY: &str = "AGENT-A-REPLY-OK: hei, tämä tuli aivoista asti";

/// Returns the final LLM reply from the outbox (excludes ack/typing/progress messages).
fn find_llm_reply(sent: &[OutboundMessage]) -> Option<&OutboundMessage> {
    sent.iter()
        .find(|m| m.kind == OutboundKind::Message && m.body == FIXED_LLM_REPLY)
}

/// Counts the final LLM replies (one inbound -> one such message).
fn count_llm_replies(sent: &[OutboundMessage]) -> usize {
    sent.iter()
        .filter(|m| m.kind == OutboundKind::Message && m.body == FIXED_LLM_REPLY)
        .count()
}

/// Starts an OpenAI-compatible mock LLM server on a random port.
/// Returns the base URL in the form `http://127.0.0.1:<port>/v1` (for the resolver).
async fn spawn_mock_llm() -> String {
    // OpenAI chat-completions-shaped response with fixed content.
    let handler = || async {
        Json(serde_json::json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": FIXED_LLM_REPLY },
                "finish_reason": "stop"
            }]
        }))
    };

    let app = Router::new().route("/v1/chat/completions", post(handler));

    // Bind to a random free port (127.0.0.1:0) and read the actual port.
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("mock-LLM bind");
    let addr = listener.local_addr().expect("mock-LLM local_addr");

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    format!("http://{addr}/v1")
}

/// **Roundtrip proof**: inbound → bus → `Agent.think()` (mock LLM) →
/// `reply_sink` → `Channel::send`. One message in, one response out with the
/// correct target, content = the mock LLM's fixed text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_message_roundtrips_to_channel_send_via_mock_llm() {
    // Serialize with other build_family tests: the restart test sets a
    // process-wide FAMILYCLAW_DATA_DIR, which must NOT leak into this
    // in-memory test (otherwise the message would be recorded under the
    // wrong disk path).
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    // 1. Bring up the mock LLM (OpenAI-compatible, fixed response).
    let api_base = spawn_mock_llm().await;

    // 2. Mock channel + injected inbound message. Keep a CLONE so we can
    //    check the outbox after build_family has consumed the channel as a
    //    `Box<dyn Channel>` (clones share the same Arc<Inner> state).
    let channel = MockChannel::new("mock-agent-a").expect("channel");
    let outbox_probe = channel.clone();

    channel
        .inject(
            InboundMessage::new("user-a", "conv-a", "generic being, are you there?")
                .expect("inbound"),
        )
        .expect("inject");
    // Close the inbound stream: the buffered message is consumed, then the
    // pump ends deterministically (doesn't depend on timing).
    channel.close_inbound();

    // 3. The resolver points the "mock" provider prefix at the mock LLM's base
    //    URL. The key is read from an env variable that is NOT set -> an empty
    //    Bearer, but the mock doesn't check auth. (LAYER A: no hardcoded key.)
    let resolver =
        EnvEndpointResolver::new().with_provider("mock", api_base, "FAMILYCLAW_MOCK_LLM_KEY_UNSET");

    // 4. The agent uses the model "mock/agent-a" -> the resolver resolves it
    //    to the mock LLM -> Agent gets Some(llm) -> think() produces fixed text.
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("mock/agent-a"));
    let soul =
        familyclaw_agent::Soul::from_essence("I am a generic FamilyClaw being for this test.");

    // 5. Reply target = that one conversation (MVP: static target).
    let reply_target = "conv-a".to_string();

    // 6. ASSEMBLE THE RUNTIME -- the same call the gateway makes.
    let runtime = build_family(
        Some("roundtrip-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        reply_target.clone(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // Bus alive + agent registered (the same readiness that /readyz reports).
    assert_eq!(
        runtime.bus().count().await.expect("count"),
        1,
        "agentti busissa"
    );

    // 7. Wait for the chain to complete: pump → handle_turn → think (HTTP) →
    //    route_reply → drain → Channel::send. Poll the outbox (max ~3s)
    //    instead of sleeping a fixed time -- robust against a slow CI.
    let mut sent = Vec::new();
    for _ in 0..60 {
        sent = outbox_probe.sent();
        if find_llm_reply(&sent).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 8. PROOF: one final response, correct target, content = mock LLM.
    assert_eq!(
        count_llm_replies(&sent),
        1,
        "yksi inbound → yksi lopullinen outbound (ack/typing sallittu, ei kahdennusta)"
    );
    let reply = find_llm_reply(&sent).expect("LLM reply in outbox");
    assert_eq!(
        reply.target, reply_target,
        "vastaus ohjautui oikeaan keskusteluun (staattinen reply_target)"
    );
    assert_eq!(
        reply.body, FIXED_LLM_REPLY,
        "vastauksen sisältö on TASAN se mitä Agent.think() sai mock-LLM:ltä"
    );

    runtime.shutdown().await;
}

/// **F1 failover proof**: when the primary endpoint is dead (connection
/// refused), but the fallback model points at a live mock LLM, the chain
/// automatically fails over to the fallback and still produces a response.
/// Before F1, the Agent only kept one client -> the primary's death killed the turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dead_primary_fails_over_to_live_fallback() {
    // Serialize with the restart test's env mutation (see the roundtrip test).
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    // 1. Live mock LLM for the fallback (returns fixed text).
    let live_base = spawn_mock_llm().await;

    // 2. Dead "primary" endpoint: bind a port and close the listener
    //    immediately, so nothing listens at the address -> reqwest gets a
    //    connection error -> failover.
    let dead_addr = {
        let l = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("dead bind");
        let addr = l.local_addr().expect("dead addr");
        drop(l); // release the port -> nothing is listening.
        addr
    };
    let dead_base = format!("http://{dead_addr}/v1");

    let channel = MockChannel::new("mock-failover").expect("channel");
    let outbox_probe = channel.clone();
    channel
        .inject(InboundMessage::new("user-a", "fo-chat", "kestääkö ketju?").expect("inbound"))
        .expect("inject");
    channel.close_inbound();

    // 3. Resolver: "dead" provider -> dead endpoint, "live" provider -> mock.
    let resolver = EnvEndpointResolver::new()
        .with_provider("dead", dead_base, "FAMILYCLAW_DEAD_KEY_UNSET")
        .with_provider("live", live_base, "FAMILYCLAW_LIVE_KEY_UNSET");

    // 4. Primary = dead/model (crashes), fallback = live/model (succeeds).
    let agent_cfg = AgentConfig::new(
        "agent_a",
        ModelConfig::new("dead/model").with_fallback("live/model"),
    );
    let soul = familyclaw_agent::Soul::from_essence("generic being for failover test");

    let reply_target = "fo-chat".to_string();
    let runtime = build_family(
        Some("failover-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        reply_target.clone(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // 5. Poll the outbox: the primary crashes, the fallback produces a response.
    let mut sent = Vec::new();
    for _ in 0..60 {
        sent = outbox_probe.sent();
        if find_llm_reply(&sent).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 6. PROOF: the response still got through (via the fallback), with the correct target.
    assert_eq!(
        count_llm_replies(&sent),
        1,
        "kuollut primary → failover fallbackiin → yksi lopullinen vastaus"
    );
    let reply = find_llm_reply(&sent).expect("LLM reply");
    assert_eq!(reply.target, reply_target, "vastaus oikeaan keskusteluun");
    assert_eq!(
        reply.body, FIXED_LLM_REPLY,
        "vastaus tuli fallback-mallilta (live mock-LLM)"
    );

    runtime.shutdown().await;
}

/// Starts a **hanging** (slow-loris) HTTP endpoint: accepts the TCP
/// connection but NEVER writes a response. This simulates a stuck primary
/// which -- unlike `dead_primary` (ECONNREFUSED) -- accepts the connection
/// but sleeps past the request timeout. Returns the base URL for the resolver.
///
/// The retained sockets are kept alive in a background task (dropping them
/// would close them and change the behavior into a connection error).
async fn spawn_hanging_llm() -> String {
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("hang bind");
    let addr = listener.local_addr().expect("hang addr");

    tokio::spawn(async move {
        let mut held = Vec::new();
        // Accept the connection, DO NOT respond, keep the socket open in
        // `held` -> the client hangs until its own request timeout fires.
        // Ends when the listener closes.
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });

    format!("http://{addr}/v1")
}

/// **F1 timeout proof (root cause):** the primary ACCEPTS the connection but
/// SLEEPS past the request timeout (slow-loris/hang). Before F1, `LlmClient`
/// built a `reqwest::Client` WITHOUT a timeout -> a hung primary blocked
/// `LlmFailover::complete()` forever and failover never triggered.
/// With this test's short timeout the primary gives up with a retryable
/// `LlmError::Timeout` error -> the chain fails over to the live fallback ->
/// the response still gets through. Unlike the `dead_primary` test, this one
/// triggers from a TIMEOUT, not just ECONNREFUSED.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_primary_fails_over_to_live_fallback() {
    // Serialize with the restart test's env mutation (see the roundtrip test).
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    // 1. Live mock LLM for the fallback + a hanging "primary".
    let live_base = spawn_mock_llm().await;
    let hanging_base = spawn_hanging_llm().await;

    let channel = MockChannel::new("mock-timeout-fo").expect("channel");
    let outbox_probe = channel.clone();
    channel
        .inject(InboundMessage::new("user-a", "to-chat", "jumittuuko ketju?").expect("inbound"))
        .expect("inject");
    channel.close_inbound();

    // 2. Resolver: "hang" provider -> hanging endpoint with a SHORT request
    //    timeout (300ms) so the test is fast; "live" provider -> mock.
    //    The timeout is set on the resolver (LAYER B path) -> it is inherited
    //    by every resolved LlmConfig = the same path the gateway runs.
    let resolver = EnvEndpointResolver::new()
        .with_request_timeout_ms(300)
        .with_connect_timeout_ms(300)
        .with_provider("hang", hanging_base, "FAMILYCLAW_HANG_KEY_UNSET")
        .with_provider("live", live_base, "FAMILYCLAW_LIVE_KEY_UNSET");

    // 3. Primary = hang/model (hangs -> timeout), fallback = live/model (succeeds).
    let agent_cfg = AgentConfig::new(
        "agent_a",
        ModelConfig::new("hang/model").with_fallback("live/model"),
    );
    let soul = familyclaw_agent::Soul::from_essence("generic being for timeout-failover test");

    let reply_target = "to-chat".to_string();
    let runtime = build_family(
        Some("timeout-failover-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        reply_target.clone(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // 4. Poll the outbox: the hung primary times out (~300ms), then the
    //    fallback produces a response. Allow a generous window (timeout + network).
    let mut sent = Vec::new();
    for _ in 0..120 {
        sent = outbox_probe.sent();
        if find_llm_reply(&sent).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 5. PROOF: the response got through via the fallback -- failover
    //    triggered from a TIMEOUT (not ECONNREFUSED), with the correct target.
    assert_eq!(
        count_llm_replies(&sent),
        1,
        "hyytynyt primary → timeout → failover fallbackiin → yksi lopullinen vastaus"
    );
    let reply = find_llm_reply(&sent).expect("LLM reply");
    assert_eq!(reply.target, reply_target, "vastaus oikeaan keskusteluun");
    assert_eq!(
        reply.body, FIXED_LLM_REPLY,
        "vastaus tuli fallback-mallilta (live mock-LLM) timeoutin jälkeen"
    );

    runtime.shutdown().await;
}

/// Confirms that without an LLM (unrecognized provider -> no think text) the
/// chain does NOT produce an outbound message -- i.e. the reply genuinely
/// comes from `think()`, not from some other source. (Negative control for
/// the roundtrip claim.)
/// **F2 proof (per-message origin routing):** one agent receives two messages
/// **from different origins** (chA/convA and chB/convB). Each response's
/// target is derived **per message** from the envelope's `origin` field ->
/// two responses route to the CORRECT targets (convA -> convA, convB ->
/// convB), no leak and neither goes to the static target.
///
/// This is F2's root-cause proof: before the origin was carried in the bus
/// envelope, the agent ALWAYS routed to the static
/// [`Agent::with_reply_target`] value, so >1 conversation leaked into the
/// same target. Now the origin carries the conversation and the reply target
/// is per message. The static target is deliberately given as different from
/// BOTH ("UNUSED-static"): if routing accidentally used it, the test would fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_origins_route_replies_to_correct_targets_no_leak() {
    use std::sync::Arc;

    use familyclaw_agent::{build_llm_chain, new_reply_channel, Agent, Soul};
    use familyclaw_bus::{BeingId, BusMessage, MessageOrigin, ResonanceBus};
    use familyclaw_durable::{DurableContext, InMemoryJournal, Journal};
    use familyclaw_memory::{LocalJsonStore, MemoryStore};

    // 1. Live mock LLM (fixed response) -- same text for both turns, so the
    //    difference shows up ONLY in the target, not the content.
    let api_base = spawn_mock_llm().await;
    let resolver =
        EnvEndpointResolver::new().with_provider("mock", api_base, "FAMILYCLAW_F2_MOCK_KEY_UNSET");

    // 2. Bus + reply sink (keep the recv end, check the targets).
    let bus = ResonanceBus::start(Some("f2-origin-bus".to_string()))
        .await
        .expect("bus");
    let (sink, mut reply_rx) = new_reply_channel();

    // 3. Agent with one model (mock LLM) + reply sink + a STATIC target that
    //    is NEITHER conversation (proves that origin wins).
    let model = ModelConfig::new("mock/agent-a");
    let chain = build_llm_chain(&model, &resolver).expect("chain builds");
    let memory: Arc<dyn MemoryStore + Send + Sync> = Arc::new(LocalJsonStore::in_memory());
    let durable =
        DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
            .expect("durable");
    let agent = Agent::new(
        AgentConfig::new("agent_a", model),
        Soul::from_essence("generic being for F2 origin routing test"),
        memory,
        durable,
        bus.clone(),
        None,
        None,
    )
    .with_reply_sink(sink)
    .with_reply_target("UNUSED-static-target")
    .with_failover(chain);
    let _actor = agent.spawn().await.expect("spawn");

    // 4. The channel's own bus seat (DIFFERENT from the agent, otherwise "its
    //    own echo" is skipped).
    let channel_seat = BeingId::new();

    // 5. Publish TWO messages from different origins, with the origin field set.
    let origin_a = MessageOrigin::new("chA", "convA", "user-a");
    let origin_b = MessageOrigin::new("chB", "convB", "user-b");
    bus.publish_with_origin(channel_seat, BusMessage::text("viesti A:lta"), origin_a)
        .expect("publish A");
    bus.publish_with_origin(channel_seat, BusMessage::text("viesti B:lta"), origin_b)
        .expect("publish B");

    // 6. Collect two final responses (ack/typing are skipped).
    let mut targets = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while targets.len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timeout waiting for two LLM replies"
        );
        while let Ok(out) = reply_rx.try_recv() {
            if out.kind == OutboundKind::Message && out.body == FIXED_LLM_REPLY {
                targets.push(out.target);
            }
        }
        if targets.len() < 2 {
            if let Ok(Some(out)) =
                tokio::time::timeout(Duration::from_millis(100), reply_rx.recv()).await
            {
                if out.kind == OutboundKind::Message && out.body == FIXED_LLM_REPLY {
                    targets.push(out.target);
                }
            }
        }
    }
    targets.sort();

    // 7. PROOF: two responses, targets = convA and convB (per-message origin),
    //    NOT "UNUSED-static-target", no leak (each exactly once).
    assert_eq!(
        targets,
        vec!["convA".to_string(), "convB".to_string()],
        "vastaukset ohjautuivat per-viesti-originin kohteisiin, ei staattiseen"
    );

    bus.stop();
}

/// Confirms that without an LLM (unrecognized provider -> no think text) the
/// chain does NOT produce an outbound message -- i.e. the reply genuinely
/// comes from `think()`, not from some other source. (Negative control for
/// the roundtrip claim.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_llm_no_reply_is_emitted() {
    // Serialize with the restart test's env mutation: this test assumes the
    // in-memory path (no FAMILYCLAW_DATA_DIR) -> must not leak a disk path.
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    let channel = MockChannel::new("mock-nollm").expect("channel");
    let outbox_probe = channel.clone();
    channel
        .inject(InboundMessage::new("u", "c", "moi ilman aivoja").expect("inbound"))
        .expect("inject");
    channel.close_inbound();

    // Empty resolver -> provider doesn't resolve -> build_llm_chain Err -> no LLM.
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("unknown/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being");

    let runtime = build_family(
        None,
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "c".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // Let the chain run -- the message gets pumped; the turn watchdog sends a
    // silence warning because there is no LLM and no real response is produced.
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        outbox_probe.sent_count(),
        1,
        "ilman LLM:ää turn-watchdog lähettää fallback-vastauksen (ei hiljaista kuolemaa)"
    );

    runtime.shutdown().await;
}

/// Serializes tests that depend on `FAMILYCLAW_DATA_DIR`: the env variable is
/// process-wide, so two concurrent tests would interfere with each other.
///
/// **Async** mutex (not `std::sync::Mutex`): its guard is `Send`, so it can be
/// held across `.await` points in a `multi_thread` tokio test without the
/// future becoming `!Send`.
static DATA_DIR_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Unique temporary directory for this test run (no external crates).
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "familyclaw-runtime-restart-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let _ = std::fs::remove_dir_all(&p);
    p
}

/// Sets up a persistent runtime in the given data directory, feeds it one
/// message, waits for the chain to process it, and shuts down cleanly.
/// Returns how many outbound responses were sent to the channel.
async fn run_one_persistent_turn(data_dir: &std::path::Path, body: &str, api_base: &str) -> usize {
    let channel = MockChannel::new("mock-restart").expect("channel");
    let outbox_probe = channel.clone();
    channel
        .inject(InboundMessage::new("user-a", "restart-chat", body).expect("inbound"))
        .expect("inject");
    channel.close_inbound();

    let resolver = EnvEndpointResolver::new().with_provider(
        "mock",
        api_base.to_string(),
        "FAMILYCLAW_RESTART_KEY_UNSET",
    );
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("mock/agent-a"));
    let soul = familyclaw_agent::Soul::from_essence("generic being for restart test");

    // Set the data dir for the duration of this build_family call (the lock is
    // held by the caller; no concurrent env readers). Edition 2021: `set_var` is safe.
    std::env::set_var("FAMILYCLAW_DATA_DIR", data_dir);
    std::env::set_var("FAMILYCLAW_DREAM_DISABLED", "1");

    let runtime = build_family(
        Some(format!("restart-bus-{body}")),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "restart-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // Poll the outbox: pump → handle_turn → think (HTTP) → reply → send.
    let mut sent = Vec::new();
    for _ in 0..80 {
        sent = outbox_probe.sent();
        if find_llm_reply(&sent).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Let the durable+memory write settle to disk before shutting down.
    tokio::time::sleep(Duration::from_millis(50)).await;
    runtime.shutdown().await;
    count_llm_replies(&sent)
}

/// **Gateway restart proof (blocker regression):** build a persistent
/// runtime, feed a message, shut down, build it AGAIN from the same
/// `FAMILYCLAW_DATA_DIR`, and feed a NEW message. Before the fix, the agent's
/// `turn_counter` stayed at zero and the durable cursor was still in replay
/// mode -> the second live message hit the replayed `turn-0` -> the agent
/// went mute (no reply) and the new message's memory was lost (`turn_key`
/// collision -> dedup). After the fix ([`Agent::resume_live`]), the second
/// message is processed FRESH: a new reply is sent AND a new memory row is
/// created (2 total).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_restart_processes_new_message_fresh_not_replayed_mute() {
    use familyclaw_memory::{LocalJsonStore, MemoryStore};

    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    let api_base = spawn_mock_llm().await;
    let data_dir = unique_temp_dir("mute");

    // --- Run 1: the first message into the persistent runtime. ---
    let sent_1 = run_one_persistent_turn(&data_dir, "ensimmäinen viesti", &api_base).await;
    assert_eq!(
        sent_1, 1,
        "ajo 1: ensimmäinen viesti tuottaa yhden vastauksen"
    );

    // Exactly one row in memory after run 1 (read back from disk).
    let mem_after_1 = LocalJsonStore::open(data_dir.join("memory.json"))
        .await
        .expect("open mem 1");
    assert_eq!(
        mem_after_1.len().await.expect("len 1"),
        3,
        "ajo 1: vuosimuisti + chat user + chat assistant"
    );

    // --- Run 2: a NEW message, same data dir -> restart scenario. ---
    let sent_2 = run_one_persistent_turn(&data_dir, "toinen UUSI viesti", &api_base).await;

    // CORE CLAIM 1: the new message is NOT muted into replay -- a response is sent.
    assert_eq!(
        sent_2, 1,
        "ajo 2: restartin jälkeen UUSI viesti käsitellään tuoreena (ei replay-mykkyyttä)"
    );

    // CORE CLAIM 2: the new message produces a NEW memory row (no turn_key collision).
    let mem_after_2 = LocalJsonStore::open(data_dir.join("memory.json"))
        .await
        .expect("open mem 2");
    assert_eq!(
        mem_after_2.len().await.expect("len 2"),
        6,
        "ajo 2: toinen vuoro lisää kolme muistoriviä (ei turn_key-törmäystä)"
    );

    // Cleanup (edition 2021: `remove_var` is safe).
    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// **Defect #1 proof (suspend/resume crash resilience on the production
/// path):** when `build_family` is run on a persistent path
/// (`FAMILYCLAW_DATA_DIR` set), the agent's resumable-turns surface is wired
/// to the CRASH-SURVIVING `JournalResumableStore` at
/// `<data_dir>/resumable.jsonl` -- not left on the in-memory default. Before
/// the fix, the only production path (`build_family`, which the gateway
/// calls) left the agent on its default (`InMemoryResumableStore`), so every
/// pending resumable turn was lost on restart.
///
/// The proof is deliberately strict but indirect: we don't have access to the
/// agent's internal surface (the agent moves into an actor), but
/// `JournalResumableStore::open` CREATES the journal file when opening it. On
/// the persistent path the file must be created; on the in-memory path (the
/// same test without a data dir) it must not be created. Reopening the same
/// data dir (restart) preserves the file -- the surface is shared and
/// persistent, not per-process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_family_wires_durable_resumable_store_on_persistent_path() {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    let api_base = spawn_mock_llm().await;
    let data_dir = unique_temp_dir("resumable");
    let resumable_path = data_dir.join("resumable.jsonl");

    // --- Persistent path: resumable.jsonl must be created. ---
    let sent = run_one_persistent_turn(&data_dir, "viesti yksi", &api_base).await;
    assert_eq!(sent, 1, "persistentti vuoro tuottaa vastauksen");
    assert!(
        resumable_path.is_file(),
        "build_family wires JournalResumableStore on persistent path → resumable.jsonl must exist at {}",
        resumable_path.display()
    );

    // --- Restart (same data dir): the file survives, doesn't disappear across a process. ---
    let sent_2 = run_one_persistent_turn(&data_dir, "viesti kaksi", &api_base).await;
    assert_eq!(sent_2, 1, "restartin jälkeinen vuoro tuottaa vastauksen");
    assert!(
        resumable_path.is_file(),
        "durable resumable journal survives restart (shared, persistent — not per-process)"
    );

    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// **Defect #1 counter-proof (in-memory path):** without
/// `FAMILYCLAW_DATA_DIR`, the agent stays on the in-memory default and no
/// resumable journal is written to disk -- backward-compatible, no
/// filesystem side effects.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_family_in_memory_path_writes_no_resumable_journal() {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    // Make sure a previous test didn't leave the env variable set.
    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");

    let channel = MockChannel::new("mock-inmem-resumable").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being");

    let runtime = build_family(
        None,
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "c".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // Bus running, one being -- the assembly was built with in-memory surfaces.
    assert_eq!(runtime.bus().count().await.expect("count"), 1);
    runtime.shutdown().await;
}

/// **Exactly-once proof (dispatch outbox on the production path):** when
/// `build_family` is run on a persistent path (`FAMILYCLAW_DATA_DIR` set),
/// the agent's shared action runtime gets a CRASH-SURVIVING dispatch outbox
/// (`JournalDispatchOutbox`, `<data_dir>/dispatch_outbox.jsonl`) in place of
/// the in-memory default. Before the fix, the only production path
/// (`build_family`, which the gateway calls) left the outbox on its default
/// (`InMemoryDispatchOutbox`), so `submit_task`'s exactly-once guarantee died
/// in exactly the SIGKILL crash the outbox exists to survive.
///
/// The proof is twofold: (1) direct -- the shared action runtime's
/// `dispatch_outbox_kind()` is `"journal"` (not `"in-memory"`); (2) indirect --
/// `JournalDispatchOutbox::open` CREATES the journal file, so
/// `<data_dir>/dispatch_outbox.jsonl` must be created and must survive a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_family_wires_durable_dispatch_outbox_on_persistent_path() {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    let data_dir = unique_temp_dir("dispatch-outbox");
    let outbox_path = data_dir.join("dispatch_outbox.jsonl");
    std::fs::create_dir_all(&data_dir).expect("temp dir");

    std::env::set_var("FAMILYCLAW_DATA_DIR", &data_dir);
    std::env::set_var("FAMILYCLAW_DREAM_DISABLED", "1");

    let channel = MockChannel::new("mock-dispatch-outbox").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being for dispatch outbox test");

    let runtime = build_family(
        Some("dispatch-outbox-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "dispatch-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // Direct claim: the shared action runtime carries a journal outbox.
    {
        let actions = runtime.actions();
        let guard = actions.lock().await;
        assert_eq!(
            guard.dispatch_outbox_kind(),
            "journal",
            "persistent build wires JournalDispatchOutbox (crash-surviving exactly-once)"
        );
    }
    // Indirect claim: the file was created on opening.
    assert!(
        outbox_path.is_file(),
        "build_family wires JournalDispatchOutbox on persistent path → dispatch_outbox.jsonl must exist at {}",
        outbox_path.display()
    );

    runtime.shutdown().await;

    // Restart (same data dir): the file survives -- the surface is shared and persistent.
    let channel2 = MockChannel::new("mock-dispatch-outbox-2").expect("channel");
    channel2.close_inbound();
    let agent_cfg2 = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul2 = familyclaw_agent::Soul::from_essence("generic being for dispatch outbox test");
    let runtime2 = build_family(
        Some("dispatch-outbox-bus-2".to_string()),
        agent_cfg2,
        soul2,
        vec![],
        Box::new(channel2),
        "dispatch-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family restart");
    {
        let actions = runtime2.actions();
        let guard = actions.lock().await;
        assert_eq!(
            guard.dispatch_outbox_kind(),
            "journal",
            "restart keeps journal outbox"
        );
    }
    assert!(
        outbox_path.is_file(),
        "durable dispatch outbox journal survives restart (shared, persistent — not per-process)"
    );
    runtime2.shutdown().await;

    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// **Exactly-once counter-proof (in-memory path):** without
/// `FAMILYCLAW_DATA_DIR`, the runtime stays on the in-memory dispatch outbox
/// (`"in-memory"`) and no dispatch journal is written to disk --
/// backward-compatible, no filesystem side effects (correct: no persistence
/// was requested).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_family_in_memory_path_uses_in_memory_dispatch_outbox() {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");

    let channel = MockChannel::new("mock-inmem-dispatch").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being");

    let runtime = build_family(
        None,
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "c".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    {
        let actions = runtime.actions();
        let guard = actions.lock().await;
        assert_eq!(
            guard.dispatch_outbox_kind(),
            "in-memory",
            "in-memory build keeps InMemoryDispatchOutbox (no persistence requested)"
        );
    }
    runtime.shutdown().await;
}

/// **Counter skill (side-effect gauge) for the PRODUCT-PATH outbox test.**
///
/// Every `execute` increments a **process-internal** counter. The test
/// requires that the counter stays at **zero** when a dispatch returns as an
/// outbox replay (committed) or is rejected fail-closed (intent-only) --
/// either way proves the side effect is not re-run through [`build_family`].
///
/// The skill is deliberately **auto-run** ([`ActionRisk::ReadOnly`] +
/// [`ApprovalPolicy::AutoIfReadOnly`]), so `submit_task_idempotent` WOULD run
/// the executor immediately -- except when the outbox neutralizes it. This
/// way an "external side effect" would be measurable if a double fire got through.
#[derive(Debug)]
struct CountingSideEffect {
    /// Process-internal side-effect counter (shared across clones).
    calls: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl CountingSideEffect {
    /// Fixed identifier so the seed key and the re-run refer to the same skill.
    const SKILL_UUID: uuid::Uuid = uuid::uuid!("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");

    fn skill_id() -> familyclaw_actions::SkillId {
        familyclaw_actions::SkillId::from_uuid(Self::SKILL_UUID)
    }
}

#[async_trait::async_trait]
impl familyclaw_actions::ActionExecutor for CountingSideEffect {
    async fn execute(
        &self,
        request: familyclaw_actions::ActionRequest,
    ) -> familyclaw_actions::Result<familyclaw_actions::ActionResult> {
        // SIDE EFFECT: increment the counter. This must happen at most once.
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(familyclaw_actions::ActionResult::success(
            "counter bumped",
            serde_json::json!({ "ok": true }),
            request.now,
        ))
    }
}

impl familyclaw_actions::Skill for CountingSideEffect {
    fn manifest(&self) -> familyclaw_actions::manifest::SkillManifest {
        familyclaw_actions::manifest::SkillManifest {
            id: Self::skill_id(),
            name: "counting_side_effect_runtime".to_string(),
            version: "1.0.0".to_string(),
            description: "Kasvattaa sivuvaikutuslaskuria (auto-run, testikäyttö).".to_string(),
            permissions: vec![familyclaw_actions::policy::SkillPermission::NetworkRead],
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

/// **PRODUCT-PATH crash-survival integration test (P0-4 / required by GPT-5.5).**
///
/// This is the test GPT-5.5 required: the at-most-once guarantee proved on
/// **the path a real user runs** ([`build_family`] + `FAMILYCLAW_DATA_DIR`),
/// NOT just with a direct [`ActionRuntime`] harness. It models a crash
/// **between the dispatch send and the agent layer's journal append**: the
/// outbox has already been written to disk, but the process died before the
/// agent got to journal the turn. After a restart, the agent's fresh branch
/// would re-run the SAME dispatch with the same idempotency key -- and
/// **without** a crash-surviving outbox it would trigger the side effect a
/// second time.
///
/// ## What this proves THROUGH [`build_family`]
/// 1. **Seed (a crash before restart):** write two keys to the
///    crash-surviving outbox (`<data_dir>/dispatch_outbox.jsonl`) -- one
///    **committed** (the side effect happened + committed) and one
///    **intent-only** (the side effect happened, committed did NOT) --
///    without [`build_family`], directly with [`JournalDispatchOutbox`]. This
///    mimics the previous process's disk footprint after a SIGKILL.
/// 2. **Restart on the real production path:** run [`build_family`] with the
///    SAME `FAMILYCLAW_DATA_DIR` -> it wires up a `JournalDispatchOutbox`
///    that reconstructs the seeded keys from disk.
/// 3. **The side effect does NOT re-run:**
///    - committed key -> `submit_task_idempotent` returns the **value-identical**
///      stored outcome (same `task_id`) without running the executor
///      (the counter stays at 0).
///    - intent-only key -> `submit_task_idempotent` **fails closed**
///      ([`ActionError::PolicyDenied`]) without running the executor (the counter stays at 0).
///
/// The counter stays at **exactly zero** the whole time: at-most-once holds
/// on exactly the path a real user uses to build a family -- not only with a
/// unit-test harness.
// A linear seed->restart->proof sequence reads top to bottom; splitting into
// helper functions would break up the product-path narrative without a clarity benefit.
#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn product_path_build_family_honors_persisted_dispatch_outbox_no_double_fire() {
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    use familyclaw_actions::dispatch_outbox::{
        DispatchOutboxStore, DispatchedOutcome, JournalDispatchOutbox,
    };
    use familyclaw_actions::task::TaskStatus;
    use familyclaw_actions::{ActionError, ActionTaskId, SkillId};

    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");

    let data_dir = unique_temp_dir("product-path-outbox");
    std::fs::create_dir_all(&data_dir).expect("temp dir");
    let outbox_path = data_dir.join("dispatch_outbox.jsonl");

    // Stable idempotency keys (= the agent's `turn-{turn}-dispatch-{k}`).
    let committed_key = "turn-7-dispatch-0";
    let intent_only_key = "turn-9-dispatch-0";

    // --- 1) SEED: a crash footprint before restart (directly into the outbox). ---
    // A known outcome whose value-identity we can check on replay.
    let committed_task_id = ActionTaskId::new();
    let committed_outcome = DispatchedOutcome {
        task_id: committed_task_id,
        status: TaskStatus::Done,
        pending_approval: None,
        error: None,
    };
    {
        let seed = JournalDispatchOutbox::open(&outbox_path).expect("seed open");
        // committed key: intent + committed (the side effect happened + committed).
        seed.record_intent(committed_key)
            .expect("seed intent (committed)");
        seed.record_committed(committed_key, &committed_outcome)
            .expect("seed committed");
        // intent-only key: ONLY intent (crashed mid-side-effect).
        seed.record_intent(intent_only_key)
            .expect("seed intent-only");
    } // drop = "crash"; the disk footprint remains.
    assert!(
        outbox_path.is_file(),
        "seedattu dispatch_outbox.jsonl on levyllä"
    );

    // --- 2) RESTART ON THE REAL PRODUCTION PATH: build_family with the same data dir. ---
    std::env::set_var("FAMILYCLAW_DATA_DIR", &data_dir);
    std::env::set_var("FAMILYCLAW_DREAM_DISABLED", "1");

    let channel = MockChannel::new("mock-product-path-outbox").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being for product-path outbox test");

    let runtime = build_family(
        Some("product-path-outbox-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "outbox-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family on seeded data_dir");

    // build_family wired up the CRASH-SURVIVING outbox (not in-memory) ->
    // the seeded keys were reconstructed from disk.
    let actions = runtime.actions();
    {
        let guard = actions.lock().await;
        assert_eq!(
            guard.dispatch_outbox_kind(),
            "journal",
            "tuotantopolku kytkee JournalDispatchOutboxin (kaatumiskestävä at-most-once)"
        );
    }

    // Register the counter skill into the shared action runtime (the same
    // Arc<Mutex> the tool loop owns). The side-effect counter is shared via a clone.
    let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let skill_id: SkillId = CountingSideEffect::skill_id();
    {
        let mut guard = actions.lock().await;
        guard
            .register_skill(CountingSideEffect {
                calls: Arc::clone(&calls),
            })
            .expect("register counting skill into shared runtime");
    }

    let now = familyclaw_core::time::from_unix_secs(1_700_000_500).expect("ts");
    let payload = serde_json::json!({ "n": 1 });

    // --- 3a) committed key: replay returns a value-identical outcome, does
    //         NOT run the side effect (the agent's fresh-branch re-run). ---
    let replayed = {
        let mut guard = actions.lock().await;
        guard
            .submit_task_idempotent(committed_key, "agent_a", skill_id, payload.clone(), now)
            .await
            .expect("committed key replays prior outcome (no re-execute)")
    };
    assert_eq!(
        replayed.task_id, committed_task_id,
        "committed-avain palautti ARVO-IDENTTISEN lopputuloksen (sama task_id) — ei tuoretta ajoa"
    );
    assert_eq!(replayed.status, TaskStatus::Done);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "committed-replay EI ajanut sivuvaikutusta uudelleen (laskuri 0)"
    );

    // --- 3b) intent-only key: fail-closed (PolicyDenied), does NOT run the side effect. ---
    let denied = {
        let mut guard = actions.lock().await;
        guard
            .submit_task_idempotent(intent_only_key, "agent_a", skill_id, payload, now)
            .await
            .expect_err("intent-only key must fail closed (not re-execute)")
    };
    assert!(
        matches!(denied, ActionError::PolicyDenied(_)),
        "intent-only-kaatuminen → fail-closed PolicyDenied, ei sokeaa uudelleenajoa (sai: {denied:?})"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "intent-only fail-closed EI ajanut sivuvaikutusta (laskuri yhä 0 — at-most-once säilyi build_family:n läpi)"
    );

    runtime.shutdown().await;

    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// Separately confirms that inbound really **travels through the bus to the
/// agent** (memory gets an entry), independent of the reply path -- the other
/// end of the roundtrip's proof chain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_reaches_agent_over_bus() {
    // Serialize with the restart test's env mutation (the in-memory path is expected).
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    let channel = MockChannel::new("mock-bus").expect("channel");
    channel
        .inject(InboundMessage::new("u", "c", "muistatko tämän viestin").expect("inbound"))
        .expect("inject");
    channel.close_inbound();

    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being");

    let runtime = build_family(
        None,
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "c".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    tokio::time::sleep(Duration::from_millis(300)).await;

    let beings = runtime.bus().beings().await.expect("beings");
    assert_eq!(beings.len(), 1);
    assert_eq!(beings[0].name, "agent_a");

    runtime.shutdown().await;
}

/// **Durable pending surface on the production path (review finding:
/// "production wires durable outbox but leaves pending store in-memory").**
/// When `build_family` is run on a persistent path (`FAMILYCLAW_DATA_DIR`
/// set), the action runtime shared by the agent and the operator surface
/// gets a CRASH-SURVIVING pending-approvals surface (`JournalPendingStore`,
/// `<data_dir>/pending_approvals.jsonl`) in place of the in-memory default.
///
/// Before the fix, `build_family` wired the crash-surviving dispatch outbox
/// but left the pending surface in memory -> after a restart, a still-pending
/// approval disappeared from the in-memory map and `approve` returned
/// `ApprovalMissing` (404) already BEFORE the outbox's InProgress/Committed
/// guard. At-most-once then held ONLY as a (accidental) side effect of the
/// 404, not because the durable layer enforces it.
///
/// The proof is twofold, as in the dispatch-outbox test: (1) direct -- the
/// shared action runtime's `pending_store_kind()` is `"journal"` (not
/// `"in-memory"`); (2) indirect -- `JournalPendingStore::open` CREATES the
/// journal file, so `<data_dir>/pending_approvals.jsonl` must be created and
/// must survive a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_family_wires_durable_pending_store_on_persistent_path() {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    let data_dir = unique_temp_dir("pending-store");
    let pending_path = data_dir.join("pending_approvals.jsonl");
    std::fs::create_dir_all(&data_dir).expect("temp dir");

    std::env::set_var("FAMILYCLAW_DATA_DIR", &data_dir);
    std::env::set_var("FAMILYCLAW_DREAM_DISABLED", "1");

    let channel = MockChannel::new("mock-pending-store").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being for pending store test");

    let runtime = build_family(
        Some("pending-store-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "pending-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // Direct claim: the shared action runtime carries a journal pending surface.
    {
        let actions = runtime.actions();
        let guard = actions.lock().await;
        assert_eq!(
            guard.pending_store_kind(),
            "journal",
            "persistent build wires JournalPendingStore (crash-surviving pending approvals)"
        );
    }
    // Indirect claim: the file was created on opening.
    assert!(
        pending_path.is_file(),
        "build_family wires JournalPendingStore on persistent path → pending_approvals.jsonl must exist at {}",
        pending_path.display()
    );

    runtime.shutdown().await;

    // Restart (same data dir): the file survives -- the surface is shared and persistent.
    let channel2 = MockChannel::new("mock-pending-store-2").expect("channel");
    channel2.close_inbound();
    let agent_cfg2 = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul2 = familyclaw_agent::Soul::from_essence("generic being for pending store test");
    let runtime2 = build_family(
        Some("pending-store-bus-2".to_string()),
        agent_cfg2,
        soul2,
        vec![],
        Box::new(channel2),
        "pending-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family restart");
    {
        let actions = runtime2.actions();
        let guard = actions.lock().await;
        assert_eq!(
            guard.pending_store_kind(),
            "journal",
            "restart keeps journal pending store"
        );
    }
    assert!(
        pending_path.is_file(),
        "durable pending journal survives restart (shared, persistent — not per-process)"
    );
    runtime2.shutdown().await;

    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// **Pending-surface counter-proof (in-memory path):** without
/// `FAMILYCLAW_DATA_DIR`, the action runtime stays on the in-memory pending
/// surface (`"in-memory"`) -- backward-compatible, no filesystem side
/// effects (correct: no persistence was requested).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_family_in_memory_path_uses_in_memory_pending_store() {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");

    let channel = MockChannel::new("mock-inmem-pending").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being");

    let runtime = build_family(
        None,
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "c".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    {
        let actions = runtime.actions();
        let guard = actions.lock().await;
        assert_eq!(
            guard.pending_store_kind(),
            "in-memory",
            "in-memory build keeps InMemoryPendingStore (no persistence requested)"
        );
    }
    runtime.shutdown().await;
}

/// **PRODUCT-PATH pending-reload integration test (core review finding).**
///
/// Proves that a pending approval, written to the crash-surviving pending
/// surface BEFORE a "restart", IS LOADED back by a fresh `build_family` from
/// the same `FAMILYCLAW_DATA_DIR` -- so `approve` no longer returns
/// `ApprovalMissing` (404) for a still-pending approval after a restart. This
/// is the difference the fix makes: the at-most-once boundary is no longer
/// held accidentally via the 404, but the still-pending approval genuinely
/// advances to the outbox's InProgress/Committed guard.
///
/// 1. **Seed (a crash before restart):** write one pending approval to the
///    crash-surviving pending surface (`<data_dir>/pending_approvals.jsonl`)
///    -- directly with [`JournalPendingStore`], without `build_family`. This
///    mimics the previous process's disk footprint after a SIGKILL.
/// 2. **Restart on the real production path:** run [`build_family`] with the
///    SAME `FAMILYCLAW_DATA_DIR` -> it wires up a `JournalPendingStore` that
///    reconstructs the seeded approval from disk.
/// 3. **The approval is still pending:** the shared action runtime's
///    `try_pending_approvals()` lists the seeded `approval_id` -- the same
///    identifier `approve` would find (not `ApprovalMissing`).
// A linear seed->restart->proof sequence reads top to bottom.
#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn product_path_build_family_reloads_pending_approval_after_restart() {
    use familyclaw_actions::approval::Approval;
    use familyclaw_actions::pending_store::{
        JournalPendingStore, PendingApprovalStore, PendingRecord,
    };
    use familyclaw_actions::{ActionId, ActionTaskId, ApprovalId};

    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");

    let data_dir = unique_temp_dir("product-path-pending");
    std::fs::create_dir_all(&data_dir).expect("temp dir");
    let pending_path = data_dir.join("pending_approvals.jsonl");

    // Timestamps: granted in the past, expires far in the future, so
    // eviction doesn't drop the seeded approval on restart.
    let granted_at = familyclaw_core::time::from_unix_secs(1_700_000_000).expect("granted ts");
    let expires_at = familyclaw_core::time::from_unix_secs(4_000_000_000).expect("expiry ts");

    // --- 1) SEED: a still-pending approval to disk before restart. ---
    let task_id = ActionTaskId::new();
    let approval_id = {
        let approval = Approval {
            id: ApprovalId::new(),
            action_id: ActionId::new(),
            payload_hash: "0".repeat(64),
            granted_at,
            expires_at,
            consumed: false,
        };
        let id = approval.id;
        let record = PendingRecord::new(
            task_id,
            approval,
            "generic skill awaiting human approval",
            granted_at,
        );
        let seed = JournalPendingStore::open(&pending_path).expect("seed open");
        seed.insert(record).expect("seed insert");
        id
    }; // drop = "crash"; the disk footprint remains.
    assert!(
        pending_path.is_file(),
        "seedattu pending_approvals.jsonl on levyllä"
    );

    // --- 2) RESTART ON THE REAL PRODUCTION PATH: build_family with the same data dir. ---
    std::env::set_var("FAMILYCLAW_DATA_DIR", &data_dir);
    std::env::set_var("FAMILYCLAW_DREAM_DISABLED", "1");

    let channel = MockChannel::new("mock-product-path-pending").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being for product-path pending test");

    let runtime = build_family(
        Some("product-path-pending-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "pending-reload-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family on seeded data_dir");

    let actions = runtime.actions();
    {
        let guard = actions.lock().await;
        // build_family wired up the CRASH-SURVIVING pending surface (not in-memory).
        assert_eq!(
            guard.pending_store_kind(),
            "journal",
            "tuotantopolku kytkee JournalPendingStoren (kaatumiskestävä pending)"
        );
        // CORE CLAIM: the seeded, still-pending approval WAS LOADED from disk
        // -> approve would NOT see ApprovalMissing. The list has exactly that identifier.
        let pending = guard.try_pending_approvals().expect("list pending");
        assert!(
            pending
                .iter()
                .any(|p| p.approval_id == approval_id && p.task_id == task_id),
            "vielä odottava hyväksyntä rekonstruoitui restartissa (ei ApprovalMissing): \
             approval_id={approval_id} task_id={task_id}, listalla={pending:?}"
        );
    }

    runtime.shutdown().await;

    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");
    let _ = std::fs::remove_dir_all(&data_dir);
}
