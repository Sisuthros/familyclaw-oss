//! # familyclaw-runtime
//!
//! **Runtime assembly** — the C5 seam of the `FamilyClaw` platform (Layer A,
//! OSS): it wires the previously built pieces into a single living being:
//!
//! ```text
//! Channel::receive() ─► pump_channel_to_bus ─► Resonance Bus ─► Agent(spawn)
//!                                                                    │
//!                       Channel::send ◄── reply_rx ◄── route_reply ◄─┘
//! ```
//!
//! [`build_family`] is a single call that replaces the gateway's direct
//! [`ResonanceBus::start`] bootstrapping: it starts the bus, spawns the
//! agent, pumps the channel's inbound stream into the bus, and drains the
//! agent's reply queue back to the channel. [`FamilyRuntime`] owns
//! everything so that shutdown ([`FamilyRuntime::shutdown`]) is clean.
//!
//! ## MVP scope
//! One agent, one channel, a **static** reply target
//! ([`Agent::with_reply_target`]). This is exactly correct when there is
//! **one channel and one conversation**: all replies route to that single
//! target. As soon as there is more than one channel or conversation, a
//! static target would route incorrectly (reply to A within B's
//! conversation) and a per-message origin (`MessageOrigin`) is needed —
//! see [`build_family`]'s "Production boundary".
//!
//! ## OSS boundary (Layer A)
//! This crate does not hardcode family members' names, keys, models, or
//! paths. The agent's name, model, soul, channel, and reply target are all
//! supplied at runtime by the caller (the gateway reads them from the
//! environment — Layer B).

use std::env;
use std::sync::Arc;

pub mod dream_skill;
pub mod subagent;

use dream_skill::DreamSkill;
use familyclaw_actions::{
    ActionRuntime, AuditCollector, FileWriteConfig, FsReadConfig, ShellExecConfig,
};
use familyclaw_agent::{
    build_llm_chain, new_reply_channel, resolve_profile_dir, Agent, EmotionCalibration,
    ErasedMemoryStore, JournalResumableStore, LlmEndpointResolver, MetricEvent, ResumableTurnStore,
    Soul, TableCalibration,
};
use familyclaw_bridge::{AgentInfo, AgentRole, Event, EventKind, FamilyBridge, HostKind};
use familyclaw_bus::{BeingId, BusHandle, ResonanceBus, ResonanceMessage};
use familyclaw_channels::Channel;
use familyclaw_core::{time, AgentConfig, FamilyClawError, Result};
use familyclaw_durable::{DurableContext, FileJournal, InMemoryJournal, Journal};
use familyclaw_embeddings::{DeterministicEmbedder, EmbeddingProvider};
use familyclaw_memory::{EmbeddingMemoryStore, LocalJsonStore};
use familyclaw_sandbox::{default_sandbox, sandbox_availability, CodeSandbox};
use familyclaw_scheduler::runner::CancellationSignal;
use familyclaw_scheduler::{ScheduledTask, Scheduler, SchedulerHandle, SchedulerRunner};
use ractor::ActorRef;
use tokio::sync::Mutex;

/// Runtime assembly: bus + spawned agents + reply pump + channels.
///
/// Owns everything so that shutdown is clean. The `bus` handle is handed off
/// to the gateway ([`FamilyRuntime::bus`]) into its `GatewayState`; background
/// tasks (the channel→bus pump and the reply→channel drain) are kept alive via
/// [`tokio::task::JoinHandle`] handles and are stopped in
/// [`FamilyRuntime::shutdown`].
///
/// ## The reply channel is unbounded
/// The agent's reply sink ([`new_reply_channel`]) is intentionally
/// **unbounded**: [`Agent::route_reply`] is a synchronous, non-blocking call
/// (a bounded send would be async and could block the agent's turn
/// processing). Instead, the reply queue is **drained immediately** by the
/// drain task
/// (`while let Some(out) = reply_rx.recv().await { channel.send(out).await }`),
/// so messages never pile up. For high-throughput production use, add a
/// bounded wrapper or a backpressure gauge on the drain side.
pub struct FamilyRuntime {
    bus: BusHandle,
    /// **Shared observability bridge event bus.**
    ///
    /// The same [`FamilyBridge`] the caller supplied to [`build_family`] (if
    /// any). The runtime publishes runtime milestones to this bridge (currently:
    /// agent registration → [`EventKind::AgentRegistered`]), and the gateway
    /// can subscribe to it with an `EventRecorder`
    /// ([`FamilyRuntime::bridge`]) so the shared `MetricsRegistry` sees live
    /// increments. `None` when the caller didn't supply a bridge (e.g. smoke
    /// tests that don't need observability).
    ///
    /// [`EventKind::AgentRegistered`]: familyclaw_bridge::EventKind::AgentRegistered
    bridge: Option<FamilyBridge>,
    /// **Shared action runtime** (suspend/resume bridge, roadmap §6 D2).
    ///
    /// The same [`Arc<Mutex<ActionRuntime>>`] wired into the agent's tool loop
    /// ([`Agent::with_actions`]). The runtime keeps its OWN handle to this so
    /// the operator surface (gateway `GET /approvals/pending` + `POST
    /// /approvals/{id}/approve`) can read pending approvals and grant approval
    /// without exposing the agent's internals. The mutex is a
    /// `tokio::sync::Mutex` because [`ActionRuntime::approve`] is `async` +
    /// `&mut self`.
    ///
    /// The lock is held **only** for the duration of a single operation
    /// (list/approve); the agent's tool loop takes the same lock for its own
    /// calls — contention is resolved through the lock, not by copying shared
    /// state.
    actions: Arc<Mutex<ActionRuntime>>,
    /// **Shared turn-audit collector** (TURN-AUDIT, roadmap §6 D6).
    ///
    /// The same [`Arc<AuditCollector>`] wired into the agent's tool loop
    /// ([`Agent::with_turn_audit`](familyclaw_agent::Agent::with_turn_audit)).
    /// The runtime keeps its OWN handle to this so the operator surface (e.g.
    /// a gateway route) can read the observable tool-loop trace (turn start,
    /// tool calls redacted, suspend/resume, `stop_reason`) without exposing the
    /// agent's internals. The collector is thread-safe and append-only
    /// (tamper-evident); `detail` fields are already redacted at write time.
    turn_audit: Arc<AuditCollector>,
    /// Spawned agent actors. Kept alive (drop = actor stops → reply sink is
    /// dropped → the drain task naturally runs to completion).
    agents: Vec<ActorRef<ResonanceMessage>>,
    /// Reply→channel drain. Kept SEPARATE from the abortable tasks: this one
    /// carries in-flight responses, so it is DRAINED to completion (not
    /// aborted) on shutdown, so a buffered response is never lost.
    drain: tokio::task::JoinHandle<()>,
    /// Abortable background tasks: the channel→bus pump. These do NOT carry
    /// in-flight responses, so they can be aborted directly.
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Cancellation signal for the scheduler (Phase 4), if the scheduler was
    /// started (the dream cycle as a scheduled task). `None` when the
    /// scheduler is disabled (`FAMILYCLAW_DREAM_DISABLED`). Shutdown cancels
    /// it → the tick loop stops cleanly.
    scheduler_signal: Option<CancellationSignal>,
    /// Shared scheduler handle (family agency, Phase 4). `Some` when the
    /// scheduler is running → the operator surface (gateway) can toggle
    /// scheduled tasks on/off ([`Scheduler::set_task_enabled`]) through the
    /// same lock the tick loop uses. `None` when the scheduler is disabled.
    scheduler_handle: Option<SchedulerHandle>,
    /// Path to the family agency config (`<data_dir>/agency.json`), when the
    /// scheduler is running on a persistent path (Phase 4). The operator
    /// surface (gateway) writes kill-switch changes here so they survive a
    /// restart. `None` in in-memory mode or when the scheduler is disabled.
    agency_config_path: Option<std::path::PathBuf>,
}

impl FamilyRuntime {
    /// Bus handle (to be shared, e.g., into the gateway's `GatewayState`).
    #[must_use]
    pub fn bus(&self) -> &BusHandle {
        &self.bus
    }

    /// **Bridge-layer event bus** for observability (if wired).
    ///
    /// Returns `Some(&bridge)` when the caller supplied a [`FamilyBridge`] to
    /// [`build_family`]. The same clone the runtime publishes runtime
    /// milestones to (agent registration → [`EventKind::AgentRegistered`]):
    /// the gateway can subscribe to it with an `EventRecorder`, so the shared
    /// `MetricsRegistry` gets live increments. `None` when no bridge was
    /// supplied.
    ///
    /// [`EventKind::AgentRegistered`]: familyclaw_bridge::EventKind::AgentRegistered
    #[must_use]
    pub fn bridge(&self) -> Option<&FamilyBridge> {
        self.bridge.as_ref()
    }

    /// **Shared action runtime handle** for the operator surface.
    ///
    /// Returns a clone of the same [`Arc<Mutex<ActionRuntime>>`] the agent's
    /// tool loop owns ([`Agent::with_actions`]). The gateway stores this in
    /// its `GatewayState` and uses it for:
    /// - `GET /approvals/pending` → [`ActionRuntime::try_pending_approvals`] +
    ///   [`ActionRuntime::pending_summary_for`] (redacted summaries),
    /// - `POST /approvals/{id}/approve` → [`ActionRuntime::approve`] (grants
    ///   approval and runs the suspended action to completion).
    ///
    /// The handle is always present: [`build_family`] creates an action
    /// runtime (with default skills) for every family and wires the same
    /// handle into both the agent and the runtime. The lock is held only for
    /// the duration of an operation.
    #[must_use]
    pub fn actions(&self) -> Arc<Mutex<ActionRuntime>> {
        Arc::clone(&self.actions)
    }

    /// **Shared turn-audit collector handle** for the operator surface
    /// (TURN-AUDIT, roadmap §6 D6).
    ///
    /// Returns a clone of the same [`Arc<AuditCollector>`] the agent's tool
    /// loop owns ([`Agent::with_turn_audit`](familyclaw_agent::Agent::with_turn_audit)).
    /// The gateway can store this in its state and show the operator the
    /// observable tool-loop trace ([`AuditCollector::list`] /
    /// [`AuditCollector::events_for`]) — turn start, tool calls
    /// **redacted**, suspend/resume, and `stop_reason`. The output never
    /// contains raw secrets.
    #[must_use]
    pub fn turn_audit(&self) -> Arc<AuditCollector> {
        Arc::clone(&self.turn_audit)
    }

    /// **Shared scheduler handle** for the operator surface (family agency,
    /// Phase 4).
    ///
    /// Returns `Some(handle)` when the scheduler is running → the gateway can
    /// toggle scheduled tasks on/off ([`Scheduler::set_task_enabled`]) through
    /// the same lock the tick loop uses (contention is resolved via the
    /// lock). `None` when the scheduler is disabled
    /// (`FAMILYCLAW_DREAM_DISABLED`).
    #[must_use]
    pub fn scheduler_handle(&self) -> Option<SchedulerHandle> {
        self.scheduler_handle.clone()
    }

    /// **Path to the family agency config** (`<data_dir>/agency.json`) for the
    /// operator surface (Phase 4). The gateway writes kill-switch changes here
    /// so they survive a restart. `Some` when the scheduler is running on a
    /// persistent path; `None` in in-memory mode or when the scheduler is
    /// disabled.
    #[must_use]
    pub fn agency_config_path(&self) -> Option<std::path::PathBuf> {
        self.agency_config_path.clone()
    }

    /// Shuts down the assembly cleanly: **never drops in-flight responses.**
    ///
    /// The order is intentional:
    /// 1. Stop the bus + drop the agents → reply production stops and the
    ///    reply sink is dropped → the drain task's `reply_rx.recv()` returns
    ///    `None`.
    /// 2. Abort the pump (+ dream) — they carry no responses.
    /// 3. **Wait for drain to finish** (bounded timeout) → buffered responses
    ///    reach the channel before returning. Previously drain was aborted →
    ///    the last responses were lost (breaking the "clean shutdown" promise).
    pub async fn shutdown(self) {
        // 1. Stop reply production: bus stop + agents gone (drops the sink).
        self.bus.stop();
        drop(self.agents);
        // 2. Cancel the scheduler (Phase 4): the tick loop stops cleanly.
        if let Some(signal) = self.scheduler_signal {
            signal.cancel();
        }
        // 3. Abort background tasks that carry no responses.
        for t in self.tasks {
            t.abort();
        }
        // 4. Let drain run to completion (bounded, so shutdown doesn't hang).
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.drain).await;
    }
}

/// Configuration pieces for one extra agent ([`build_family`]'s `extra_agents`).
#[derive(Debug, Clone)]
pub struct AgentBuildSpec {
    /// Extra agent's configuration (model, skills, channels).
    pub config: AgentConfig,
    /// Extra agent's soul (identity, voice, boundaries).
    pub soul: Soul,
    /// Per-agent reply target; `None` -> the family's default `reply_target`.
    pub reply_target: Option<String>,
}

/// C5 assembler: builds a living [`FamilyRuntime`] with a single call.
///
/// Wires `Channel::receive()` -> [`familyclaw_agent::pump_channel_to_bus`] -> bus -> `Agent`
/// (spawn) -> `route_reply` -> reply queue -> `Channel::send`. MVP: one agent,
/// one channel, a static reply target.
///
/// The LLM is **optional**: if [`build_llm_chain`] cannot resolve any model to
/// an endpoint (e.g. a key is missing from the environment), the agent is
/// spawned without an LLM -- it remembers and reacts emotionally, but does not
/// produce text replies. This keeps the assembly startable without provider
/// keys (smoke tests, CI). Once the chain resolves, the full failover chain
/// (primary + fallbacks) is wired into the agent via [`Agent::with_failover`].
///
/// # Reply routing: per-message origin (F2) + static fallback
/// Per-message origin (`MessageOrigin`) is **fully built and tested**
/// (F2). The inbound `InboundEnvelope` carries the origin, `channel_bridge`
/// maps it to a `MessageOrigin` (`envelope_origin`), `ResonanceMessage`
/// carries it in the bus envelope (`publish_with_origin`), and
/// [`Agent::handle_turn_with_origin`] derives the reply target per message
/// from `origin.reply_target()`. The static `reply_target`/agent is now a
/// **fallback** -- it is used only when there is no origin. This way one agent
/// can serve >1 channel and >1 conversation without a reply leaking into the
/// wrong conversation. Proof: integration test
/// `two_origins_route_replies_to_correct_targets_no_leak`
/// (`familyclaw-runtime/tests/roundtrip.rs`), which deliberately sets the
/// static target wrong and proves that two different conversations route to
/// their own targets.
///
/// # Observability: `bridge` (optional)
/// The `bridge` argument is an **optional** shared bridge-layer event bus
/// ([`FamilyBridge`]). When the caller supplies it, the runtime publishes
/// runtime milestones to it -- currently **agent registration**
/// ([`EventKind::AgentRegistered`]) once the agent has been spawned (step 7d). The runtime
/// keeps the same clone ([`FamilyRuntime::bridge`]) so the gateway can subscribe to
/// it with an `EventRecorder` and populate the shared `MetricsRegistry` with live
/// numbers (e.g. the `agents_online` gauge).
///
/// **Subscription order:** the `EventBus` only delivers events published *after*
/// subscription. The caller must therefore create the `EventRecorder` (subscribe
/// to `bridge`) **before** calling this function, so the agent registration event is not
/// lost. `None` -> no publishing (smoke tests that don't need metrics).
///
/// [`EventKind::AgentRegistered`]: familyclaw_bridge::EventKind::AgentRegistered
///
/// # Errors
/// - [`FamilyClawError::Config`] if the model configuration is invalid (this
///   is only raised if the primary is empty -- a missing endpoint results in
///   an LLM-free agent, not an error).
/// - [`FamilyClawError::Bus`] if starting the bus, spawning/registering the
///   agent, or building the durable context fails.
/// - [`FamilyClawError::InvalidInput`] (converted from the channel layer) if
///   the channel's inbound stream cannot be opened.
// This is one linear sequence for assembling the family (bus -> LLM -> memory ->
// durable -> agent -> channel -> dream). The numbered steps read top to bottom;
// splitting into helper functions would break up this assembly narrative and
// add argument plumbing without a clarity benefit.
//
// 8 arguments (limit 7) is a deliberate choice: each is an independent
// assembly piece with no natural grouping into a params struct, and this
// function is the whole runtime's public entry point -- changing the
// signature would break every caller.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn build_family(
    bus_name: Option<String>,
    agent_cfg: AgentConfig,
    soul: Soul,
    extra_agents: Vec<AgentBuildSpec>,
    channel: Box<dyn Channel>,
    reply_target: String,
    resolver: &dyn LlmEndpointResolver,
    bridge: Option<FamilyBridge>,
) -> Result<FamilyRuntime> {
    // 0. Read the persistence configuration SYNCHRONOUSLY before the first
    //    `.await` point. This way the decision (persistent vs. in-memory) is
    //    made in one place and doesn't depend on whether someone changes the
    //    `FAMILYCLAW_DATA_DIR` environment variable while the bus is starting.
    let data_dir = env::var("FAMILYCLAW_DATA_DIR").ok();

    // 1. Start the Resonance Bus (the family's affective nervous system).
    let bus = ResonanceBus::start(bus_name).await?;

    // 2. Reply channel (C1 Model A): the agent pushes replies into the sink,
    //    the runtime owns the recv end and calls Channel::send (below, step 9).
    let (sink, mut reply_rx) = new_reply_channel();
    let shared_reply_sink = sink.clone();

    let primary_model = agent_cfg.model.clone();
    let default_reply = reply_target.clone();
    // 3. LLM failover chain (optional): if no model resolves to an
    //    endpoint (e.g. a key/endpoint is missing), the agent runs without
    //    an LLM. The WHOLE chain (primary + fallbacks) is built -- F1: the
    //    primary's death (timeout/HTTP/rate) no longer kills the turn; the
    //    next fallback is tried in order instead ([`Agent::think`]).
    let failover = match build_llm_chain(&agent_cfg.model, resolver) {
        Ok(chain) => Some(chain),
        Err(e) => {
            tracing::warn!(
                target: "familyclaw::llm",
                model = %agent_cfg.model.primary,
                error = %e,
                "LLM chain unresolved — agent will run MUTE (emotion/memory only, no text \
                 replies). Set FAMILYCLAW_PROVIDERS or use provider/model form (e.g. \
                 openai/gpt-4.1-mini)."
            );
            None
        }
    };

    // 4. Memory (Eternal Thread, in-memory MVP) + durable context.
    //
    //    `persistent` indicates whether the durable context was built on top of
    //    an EXISTING journal (FAMILYCLAW_DATA_DIR). Only then is there replay
    //    history to resume live from (step 6, `resume_live`). On the in-memory
    //    path the journal is always empty -> no replay -> no resume needed.
    //
    //    `resumable` is the CRASH-SURVIVING storage surface for resumable turns
    //    (suspend/resume bridge, roadmap §6). It is built **only** on the
    //    persistent path: only then does a suspended tool-loop turn awaiting
    //    approval survive on disk across a process restart (see step 7).
    //    On the in-memory path the agent stays on its default
    //    ([`InMemoryResumableStore`]) -- no disk, no crash resilience, like the
    //    rest of in-memory mode.
    //
    //    `action_data_dir` (`Some(dir)` only on the persistent path) carries the
    //    directory from which the action runtime's THREE crash-surviving surfaces
    //    are built in step 7b:
    //
    //    1. **pending approvals surface** (`<data_dir>/pending_approvals.jsonl`,
    //       `JournalPendingStore`) -- without this, after a restart `approve`
    //       would hit an empty in-memory map and return `ApprovalMissing`
    //       (404) already BEFORE the outbox's InProgress/Committed guard, so
    //       at-most-once would hold ONLY as a (accidental) side effect of the
    //       404, not because the durable layer enforces it;
    //    2. **task queue** (`<data_dir>/action_tasks.jsonl`) -- so the
    //       approvable task (payload + state) is reconstructed on restart;
    //    3. **dispatch idempotency outbox** (`<data_dir>/dispatch_outbox.jsonl`,
    //       familyclaw_actions::JournalDispatchOutbox) -- the **at-most-once**
    //       boundary's (double-fire prevention, NOT universal exactly-once
    //       completion) CRASH-SURVIVING surface:
    //       `submit_task`'s / `approve`'s side effect runs at most once
    //       across a SIGKILL crash (never twice), and an already-committed
    //       dispatch returns value-identically; in the intent-only window a
    //       crash fails closed.
    //
    //    All three surfaces are wired with ONE [`ActionRuntime::with_durable_stores`]
    //    call (it now opens the crash-surviving dispatch outbox itself from the
    //    third path -- no more separate with_dispatch_outbox chaining and no
    //    double-opening of the outbox). On the in-memory path all three stay on
    //    their defaults (no disk), like the rest of in-memory mode.
    //
    //    `resumable` is the resumable-turns surface `<data_dir>/resumable.jsonl`,
    //    wired directly into the agent (step 7).
    let (memory, durable, dream_journal, persistent, resumable, action_data_dir) =
        if let Some(data_dir) = data_dir {
            let dir = std::path::PathBuf::from(&data_dir);
            std::fs::create_dir_all(&dir).ok();
            let journal = FileJournal::open(dir.join("journal.jsonl"))
                .map_err(|e| FamilyClawError::bus(e.to_string()))?;
            let dream_j: Arc<dyn Journal + Send + Sync> = Arc::new(journal);
            let mem = LocalJsonStore::open(dir.join("memory.json"))
                .await
                .map_err(|e| FamilyClawError::bus(e.to_string()))?;
            // Phase 3: wrap memory with auto-embedding. The default provider is
            // dependency-free and deterministic (poverty-compatible);
            // `semantic_weight` defaults to 0.0, so embeddings are produced
            // but vector search only activates once the caller raises the
            // weight -> fully backward-compatible.
            let mem = EmbeddingMemoryStore::new(mem, resolve_embedder());
            let dur = DurableContext::new(Arc::clone(&dream_j))
                .map_err(|e| FamilyClawError::bus(e.to_string()))?;
            // Crash-surviving resumable-turns surface `<data_dir>/resumable.jsonl`.
            let store = JournalResumableStore::open(dir.join("resumable.jsonl"))
                .map_err(|e| FamilyClawError::bus(e.to_string()))?;
            let resumable: Arc<dyn ResumableTurnStore> = Arc::new(store);
            (
                Arc::new(mem) as ErasedMemoryStore,
                dur,
                Some(dream_j),
                true,
                Some(resumable),
                Some(dir),
            )
        } else {
            // Phase 3: same auto-embedding wrapper on the in-memory path too.
            let mem = EmbeddingMemoryStore::new(LocalJsonStore::in_memory(), resolve_embedder());
            let memory: ErasedMemoryStore = Arc::new(mem);
            let dream_j: Arc<dyn Journal + Send + Sync> = Arc::new(InMemoryJournal::new());
            let durable = DurableContext::new(Arc::clone(&dream_j))
                .map_err(|e| FamilyClawError::bus(e.to_string()))?;
            (memory, durable, Some(dream_j), false, None, None)
        };

    // 5. Anchor identity before building the agent -- AND persist it.
    //    Previously the registry was a local `let mut registry` that was dropped
    //    right after `register()`: the anchor was never saved or re-checked on
    //    boot. Now [`ensure_identity_anchor`] loads the existing registry, runs
    //    **verify_identity** on boot (a tamper alert to the log), registers the
    //    current soul, and persists it to `anchors.json`. Env-gated
    //    (`FAMILYCLAW_HEARTH_ENABLED`).
    if env::var("FAMILYCLAW_HEARTH_ENABLED").is_ok() {
        let anchor_path = env::var("FAMILYCLAW_DATA_DIR")
            .ok()
            .map(|d| std::path::PathBuf::from(d).join("anchors.json"));
        ensure_identity_anchor(&agent_cfg.name, &soul.essence, anchor_path.as_deref());
    }

    // 6. Load the emotion engine calibration from the profile directory's
    //    `calibration.json` (LAYER B data -- see [`load_profile_calibration`]).
    //    `None` -> the agent stays on neutral calibration (non-breaking).
    let calibration = load_profile_calibration(agent_cfg.profile_dir.as_deref(), &agent_cfg.name);

    // 7. Build the agent and wire the reply sink + static reply target.
    //    The LLM is given as `None` to the constructor and the WHOLE failover
    //    chain is wired separately via [`Agent::with_failover`] (if it resolved).
    //    This way the agent gets primary + fallbacks, not just the primary.
    //
    //    **Gateway restart fix:** when the durable context was built on top of
    //    an EXISTING journal (persistent path, FAMILYCLAW_DATA_DIR), it is in
    //    replay mode. The gateway serves LIVE new messages -- it does NOT
    //    re-feed history. [`Agent::resume_live`] advances the durable cursor to
    //    the end of the replay AND restores `turn_counter` to the next free
    //    turn slot, so the next live turn (a) doesn't crash on
    //    `NondeterministicReplay` / go mute (`is_replaying`), and (b) doesn't
    //    collide with a replay duplicate in memory's `turn_key`. This is done
    //    only on the persistent path -- the in-memory journal is always empty.
    let dream_store = Arc::clone(&memory);
    // Save the agent's identity before `agent_cfg` moves into `Agent::new`
    // -- the observability bridge (step 7d) publishes the registration using
    // these values.
    let agent_id = agent_cfg.id;
    let agent_name = agent_cfg.name.clone();
    let sandbox = resolve_sandbox_skills();
    let mut agent = Agent::new(agent_cfg, soul, memory, durable, bus.clone(), None, sandbox);
    // Gateway restart fix (durable replay): advance the cursor to the end of
    // the replay and restore turn_counter to the next free turn slot ONLY on
    // the persistent path (FAMILYCLAW_DATA_DIR). The in-memory journal is empty.
    if persistent {
        agent = agent.resume_live();
    }
    // Crash-surviving resumable-turns surface (suspend/resume bridge): once it
    // was built (persistent path), wire it into the agent in place of the
    // default ([`InMemoryResumableStore`]). This way a suspended tool-loop
    // turn awaiting approval survives on disk across a process crash, and
    // [`Agent::resume_approved`] can carry it to completion after a restart.
    // Without this the production daemon would lose every pending resumable
    // turn on restart (the default is in-memory).
    if let Some(resumable) = resumable {
        agent = agent.with_resumable_store(resumable);
    }
    agent = agent.with_reply_sink(sink).with_reply_target(reply_target);
    // Emotion engine calibration (LAYER B): if the profile's calibration.json
    // resolved, wire it into the governor -- otherwise the agent stays neutral.
    if let Some(calibration) = calibration {
        agent = agent.with_calibration(calibration);
    }
    if let Some(failover) = failover {
        agent = agent.with_failover(failover);
    }

    // 7b. Action runtime (ActionRuntime) -- the tool loop + the operator's
    //     approval surface share the SAME Arc<Mutex<ActionRuntime>> handle.
    //
    //     This is the wiring point for the suspend/resume bridge (roadmap §6
    //     D2): without an action runtime, the agent's `think` can never
    //     suspend to await approval (no tools -> no `Suspended`), and so there
    //     is also nothing for the operator surface to show or approve. All
    //     skills are LAYER A generic mocks, not real providers. If skill
    //     registration were to fail (it shouldn't -- the built-in manifests
    //     are validated), it fails in a controlled way with an error before
    //     spawning.
    //
    //     The same handle is given to the agent via `with_actions` (activating
    //     the tool loop) AND stored in the runtime ([`FamilyRuntime::actions`])
    //     for the operator surface -- both point to the same locked state.
    //
    //     **Crash resilience on the persistent path** -- THREE durable surfaces:
    //
    //     - **Durable pending + task** ([`ActionRuntime::with_durable_stores`]):
    //       a pending approval (`pending_approvals.jsonl`) AND an approvable
    //       task (`action_tasks.jsonl`, payload + state) SURVIVE a restart.
    //       Without durable pending, after a restart `approve` would hit an
    //       empty in-memory map and return `ApprovalMissing` (404) already
    //       BEFORE the outbox's InProgress/Committed guard -- so at-most-once
    //       would hold only as an (accidental) side effect of the 404. Durable
    //       pending loads the approval back from disk, so control ADVANCES to
    //       the outbox guard and the **durable layer** enforces the double-fire
    //       prevention.
    //     - **Durable dispatch outbox** (opened in the SAME
    //       [`ActionRuntime::with_durable_stores`] call from the third path --
    //       no more separate with_dispatch_outbox chaining): this gives
    //       `submit_task` / `approve` an at-most-once guarantee (double-fire
    //       prevention, NOT universal exactly-once completion) across a
    //       SIGKILL crash (side effect at most once; an already-committed
    //       dispatch returns value-identically; in the intent-only window a
    //       crash fails closed).
    //
    //     On the in-memory path ([`ActionRuntime::with_default_skills`], no data_dir)
    //     all three stay on their defaults -- no disk, no crash resilience,
    //     like the rest of in-memory mode.
    // Research skill (fs_read) allowlist from the LAYER B environment.
    // `build_family` (LAYER A) does not hardcode any path -- the operator
    // supplies the allowed roots in the environment, and the flagship skill
    // [`FsReadAllowlisted`] is registered with them already at registration
    // time (fixed skill-id -> cannot register twice). Without an allowlist the
    // skill stays in an empty fail-closed state (rejects all paths), so this
    // is the switch that makes FILE RESEARCH actually work.
    let fs_read_config = resolve_fs_read_config();
    if fs_read_config.is_none() {
        // Boot-time sanity: an agent that later gets a mysterious fs_read
        // denial should be diagnosable from the logs, not just the error
        // text. `None` here means the skill registers with its default
        // empty allowlist (fail-closed) — every read is rejected.
        tracing::warn!(
            target: "familyclaw::actions",
            "fs_read is fail-closed with an empty allowlist — agents cannot read any files; \
             configure allowed roots (FAMILYCLAW_FS_READ_ALLOW)"
        );
    }
    // Write skill (file_write) allowlist from the same LAYER B environment.
    // Without it, file_write stays fail-closed (rejects all writes) -- this is
    // the switch that makes FILE WRITING actually possible (a write within the
    // allowlist runs automatically, RequireApproval).
    let file_write_config = resolve_file_write_config();
    let shell_exec_config = resolve_shell_exec_config();
    let action_runtime = if let Some(dir) = action_data_dir.as_ref() {
        // Persistent path: durable pending + task + dispatch outbox with ONE
        // constructor -- `with_durable_stores` now opens the crash-surviving
        // journal outbox itself from the third path, so a separate
        // `with_dispatch_outbox` chain call is no longer needed (and the
        // outbox file is not opened twice). Default skills follow (fs_read
        // possibly with an allowlist).
        let pending_path = dir.join("pending_approvals.jsonl");
        let task_path = dir.join("action_tasks.jsonl");
        let dispatch_path = dir.join("dispatch_outbox.jsonl");
        let mut rt = ActionRuntime::with_durable_stores(pending_path, task_path, dispatch_path)
            .await
            .map_err(|e| {
                FamilyClawError::config(format!("durable action stores open failed: {e}"))
            })?;
        rt.register_default_skills_with_configs(
            fs_read_config,
            file_write_config,
            shell_exec_config,
        )
        .map_err(|e| FamilyClawError::config(format!("action runtime build failed: {e}")))?;
        if let Err(e) = register_mcp_from_env(&mut rt).await {
            tracing::warn!(
                target: "familyclaw::mcp",
                error = %e,
                "MCP skill registration from FAMILYCLAW_MCP_SERVERS failed (non-fatal)"
            );
        }
        let spawner = Arc::new(subagent::BusSubagentSpawner::new(
            bus.clone(),
            primary_model.clone(),
            Arc::new(familyclaw_agent::EnvEndpointResolver::new()),
            default_reply.clone(),
        ));
        subagent::register_spawn_subagent_skill(&mut rt, spawner)?;
        rt
    } else {
        // In-memory path: all three surfaces on their defaults.
        let mut rt = ActionRuntime::new();
        rt.register_default_skills_with_configs(
            fs_read_config,
            file_write_config,
            shell_exec_config,
        )
        .map_err(|e| FamilyClawError::config(format!("action runtime build failed: {e}")))?;
        if let Err(e) = register_mcp_from_env(&mut rt).await {
            tracing::warn!(
                target: "familyclaw::mcp",
                error = %e,
                "MCP skill registration from FAMILYCLAW_MCP_SERVERS failed (non-fatal)"
            );
        }
        let spawner = Arc::new(subagent::BusSubagentSpawner::new(
            bus.clone(),
            primary_model.clone(),
            Arc::new(familyclaw_agent::EnvEndpointResolver::new()),
            default_reply.clone(),
        ));
        subagent::register_spawn_subagent_skill(&mut rt, spawner)?;
        rt
    };
    let actions: Arc<Mutex<ActionRuntime>> = Arc::new(Mutex::new(action_runtime));
    agent = agent.with_actions(Arc::clone(&actions));

    // 7c. Turn-audit collector (TURN-AUDIT, roadmap §6 D6): the tool loop's
    //     lifecycle becomes observable. The agent writes a trace (turn start,
    //     tool calls redacted, suspend/resume, stop_reason), and the runtime
    //     keeps its own handle to the SAME Arc<AuditCollector> for the
    //     operator surface ([`FamilyRuntime::turn_audit`]). Both point to the
    //     same thread-safe, append-only surface.
    let turn_audit: Arc<AuditCollector> = Arc::new(AuditCollector::new());
    agent = agent.with_turn_audit(Arc::clone(&turn_audit));

    // 7c-and-a-half. Observability metrics bridge (Phase 2): if a bridge layer
    //      was supplied, give the agent a lightweight [`MetricEvent`] sink and
    //      bridge its events into `Custom` labels on the bridge bus, which the
    //      `EventRecorder` already maps to metrics (`agent.turn` -> `agent_turns`,
    //      `tool.call` -> `tool_calls`). This way the agent stays DECOUPLED from
    //      `MetricsRegistry` (the same decoupling as reply_sink), and the dead
    //      `agent_turns`/`tool_calls` counters start counting. Replay guarding
    //      is done in the agent (`!is_replaying()`), so only fresh events reach
    //      the bridge.
    if let Some(bridge) = &bridge {
        // Bounded channel: the agent drops overflow via `try_send` if this
        // bridge falls behind -> no memory leak on the hot path.
        let (metrics_tx, mut metrics_rx) =
            tokio::sync::mpsc::channel::<MetricEvent>(familyclaw_agent::METRIC_SINK_CAPACITY);
        agent = agent.with_metrics_sink(metrics_tx);
        let event_bus = bridge.bus().clone();
        let bridge_agent_id = agent_id;
        tokio::spawn(async move {
            while let Some(ev) = metrics_rx.recv().await {
                let label = match ev {
                    MetricEvent::TurnCompleted => "agent.turn",
                    MetricEvent::ToolDispatched => "tool.call",
                };
                event_bus.publish(Event::new(
                    EventKind::Custom(label.to_string()),
                    Some(bridge_agent_id),
                ));
            }
        });
    }

    // 7. Spawn the agent as an actor (registers it on the bus).
    let actor = agent.spawn().await?;
    let mut agents = vec![actor];

    // 7d. Observability bridge (optional): register the primary agent.
    if let Some(bridge) = &bridge {
        let info = AgentInfo::new(agent_id, &agent_name, AgentRole::Executor, HostKind::Local);
        if let Err(e) = bridge.register_agent(info).await {
            tracing::warn!(
                target: "familyclaw::observability",
                agent = %agent_name,
                error = %e,
                "agent registration on observability bridge failed (non-fatal) — \
                 agents_online gauge will not reflect this agent"
            );
        }
    }

    // 7e. Extra agents (multi-agent serve): same bus, shared actions/audit, own soul/reply.
    for spec in extra_agents {
        let extra_id = spec.config.id;
        let extra_name = spec.config.name.clone();
        let extra_reply = spec
            .reply_target
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| default_reply.clone());
        let extra_failover = match build_llm_chain(&spec.config.model, resolver) {
            Ok(chain) => Some(chain),
            Err(e) => {
                tracing::warn!(
                    target: "familyclaw::llm",
                    agent = %extra_name,
                    error = %e,
                    "extra agent LLM chain unresolved — running without text replies"
                );
                None
            }
        };
        let extra_mem: ErasedMemoryStore = Arc::new(EmbeddingMemoryStore::new(
            LocalJsonStore::in_memory(),
            resolve_embedder(),
        ));
        let extra_journal: Arc<dyn Journal + Send + Sync> = dream_journal
            .clone()
            .unwrap_or_else(|| Arc::new(InMemoryJournal::new()));
        let extra_durable = DurableContext::new(Arc::clone(&extra_journal))
            .map_err(|e| FamilyClawError::bus(e.to_string()))?;
        let mut extra_agent = Agent::new(
            spec.config,
            spec.soul,
            extra_mem,
            extra_durable,
            bus.clone(),
            None,
            None,
        )
        .with_reply_sink(shared_reply_sink.clone())
        .with_reply_target(extra_reply)
        .with_actions(Arc::clone(&actions))
        .with_turn_audit(Arc::clone(&turn_audit));
        if persistent {
            extra_agent = extra_agent.resume_live();
        }
        if let Some(failover) = extra_failover {
            extra_agent = extra_agent.with_failover(failover);
        }
        let extra_actor = extra_agent.spawn().await?;
        if let Some(bridge) = &bridge {
            let info = AgentInfo::new(extra_id, &extra_name, AgentRole::Executor, HostKind::Local);
            if let Err(e) = bridge.register_agent(info).await {
                tracing::warn!(
                    target: "familyclaw::observability",
                    agent = %extra_name,
                    error = %e,
                    "extra agent registration on observability bridge failed (non-fatal)"
                );
            }
        }
        agents.push(extra_actor);
    }

    // 8. The channel's own bus seat -- DIFFERENT from the agent's being_id,
    //    otherwise AgentActor would skip the message as "its own echo"
    //    (agent.rs handle, sender check).
    let channel_seat = BeingId::new();

    // 9. Open the channel's inbound stream and pump it into the bus in its own
    //    task. pump_channel_to_bus blocks until the stream closes -> must be
    //    spawned.
    let stream = channel.receive().map_err(FamilyClawError::from)?;
    let pump = tokio::spawn({
        let bus = bus.clone();
        async move {
            if let Err(e) = familyclaw_agent::pump_channel_to_bus(stream, bus, channel_seat).await {
                tracing::warn!("channel→bus pump ended: {e}");
            }
        }
    });

    // 10. Drain the agent's reply queue to the channel. Share the channel via
    //    Arc -- receive() has already been called (step 8), send() goes
    //    through the Arc.
    let ch: Arc<dyn Channel> = Arc::from(channel);
    let drain = tokio::spawn(async move {
        while let Some(out) = reply_rx.recv().await {
            if let Err(e) = ch.send(out).await {
                tracing::warn!("channel send failed: {e}");
            }
        }
    });

    // 11. Dream cycle AS A SCHEDULED TASK (Phase 4, D5): instead of the
    //     previous hand-coded `tokio::sleep` loop, the dream cycle runs
    //     through `familyclaw-scheduler` as a [`DreamSkill`] skill. Benefits:
    //     idempotency (deterministic scheduler key), observability (goes
    //     through the action runtime), consistency (one scheduling
    //     mechanism). The scheduler gets its OWN, isolated `ActionRuntime`
    //     into which only `DreamSkill` is registered -- it does not share the
    //     agent's tool runtime. Only spawned if the journal exists AND
    //     `FAMILYCLAW_DREAM_DISABLED` is not set (backward-compatible).
    let (scheduler_signal, scheduler_handle): (
        Option<CancellationSignal>,
        Option<SchedulerHandle>,
    ) = if let Some(dream_journal) = dream_journal {
        let dream_disabled = env::var("FAMILYCLAW_DREAM_DISABLED")
            .ok()
            .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        if dream_disabled {
            tracing::info!(target: "familyclaw::dream", "scheduled dream task disabled (FAMILYCLAW_DREAM_DISABLED)");
            (None, None)
        } else {
            let interval_secs: i64 = env::var("FAMILYCLAW_DREAM_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(6 * 3600);

            // The scheduler's own action runtime: only DreamSkill.
            let mut sched_runtime = ActionRuntime::new();
            let dream_skill = DreamSkill::new(Arc::clone(&dream_store), Arc::clone(&dream_journal));
            if let Err(e) = sched_runtime.register_skill(dream_skill) {
                tracing::warn!(target: "familyclaw::dream", error = %e, "failed to register dream skill — scheduled dream disabled");
                (None, None)
            } else {
                let mut scheduler = Scheduler::new();
                // Stable task id (with_id) -> the idempotency key stays
                // stable across processes.
                let dream_task = ScheduledTask::new(
                    DreamSkill::skill_id(),
                    serde_json::json!({}),
                    chrono::Duration::seconds(interval_secs),
                    "dream",
                );
                scheduler.register(dream_task);
                // Family agency persistence (Phase 4): load <data_dir>/agency.json
                // and apply it -> the operator's kill switch survives a restart.
                // Only on the persistent path (action_data_dir = Some); in
                // in-memory mode there is nowhere to persist to.
                if let Some(dir) = action_data_dir.as_ref() {
                    let agency_path = dir.join("agency.json");
                    match familyclaw_scheduler::AgencyConfig::load(&agency_path) {
                        Ok(cfg) => {
                            cfg.register_scheduled_tasks(&mut scheduler);
                            scheduler.apply_agency_config(&cfg);
                            if !cfg.disabled.is_empty() {
                                tracing::info!(target: "familyclaw::scheduler", disabled = cfg.disabled.len(), "applied persisted agency config");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(target: "familyclaw::scheduler", error = %e, "failed to load agency config — using defaults");
                        }
                    }
                }
                // Tick interval: min(interval, 60s) so that expiry is noticed
                // in time but the tick doesn't spin needlessly often.
                let tick_secs = interval_secs.clamp(1, 60);
                #[allow(clippy::cast_sign_loss)]
                let period = std::time::Duration::from_secs(tick_secs as u64);
                let runner = SchedulerRunner::new(scheduler, sched_runtime, period);
                tracing::info!(target: "familyclaw::dream", interval_secs, "scheduled dream task active");
                // run_shared: shared handle to the scheduler -> the operator
                // surface can toggle tasks on/off (family agency kill switch).
                let (signal, handle) = runner.run_shared(time::now);
                (Some(signal), Some(handle))
            }
        }
    } else {
        (None, None)
    };

    // 12. Assemble the runtime -- owns the bus, the agent, and the background
    //     tasks. `drain` is kept SEPARATE from the abortable tasks, so
    //     shutdown can let it run to completion (in-flight responses) instead
    //     of aborting it. The scheduler's cancellation signal is stored
    //     separately and cancelled on shutdown.
    let tasks = vec![pump];
    // Agency config path: only when the scheduler is running (scheduler_handle
    // = Some) AND a persistent path exists -> the operator's kill switch can
    // be persisted.
    let agency_config_path = scheduler_handle
        .as_ref()
        .and(action_data_dir.as_ref())
        .map(|dir| dir.join("agency.json"));
    Ok(FamilyRuntime {
        bus,
        bridge,
        actions,
        turn_audit,
        agents,
        drain,
        tasks,
        scheduler_signal,
        scheduler_handle,
        agency_config_path,
    })
}

/// Loads the emotion engine calibration from the agent's profile directory's
/// `calibration.json` (LAYER B data, loaded at runtime -- not hardcoded). The
/// profile directory is resolved with the same logic as the soul
/// ([`resolve_profile_dir`]): explicit `configured` (the agent's
/// `profile_dir`) or `FAMILYCLAW_PROFILE_DIR/<agent_name>`.
///
/// Returns `None` if the file doesn't exist or parsing it fails -- in that
/// case the agent stays on neutral calibration
/// ([`NeutralCalibration`](familyclaw_agent::NeutralCalibration), the current
/// behavior). Fully non-breaking: a missing/invalid file does not crash boot.
fn load_profile_calibration(
    configured: Option<&std::path::Path>,
    agent_name: &str,
) -> Option<Box<dyn EmotionCalibration + Send + Sync>> {
    let dir = resolve_profile_dir(configured, agent_name)?;
    let path = dir.join("calibration.json");
    if !path.is_file() {
        return None;
    }
    match TableCalibration::from_path(&path) {
        Ok(cal) => {
            tracing::info!(
                path = %path.display(),
                label = cal.label(),
                "emotion calibration loaded for {agent_name}"
            );
            Some(Box::new(cal) as Box<dyn EmotionCalibration + Send + Sync>)
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "calibration.json parse failed (non-fatal) — using neutral calibration"
            );
            None
        }
    }
}

/// Resolves the flagship research skill's ([`FsReadAllowlisted`]) allowlist
/// from the environment (LAYER B data -- `build_family` does not hardcode paths).
///
/// - `FAMILYCLAW_FS_READ_ALLOW` -- a list-separated set of **allowed** roots
///   under which the agent may read files to research. The separator is the
///   platform's path-list separator (rather than the
///   [`std::path::MAIN_SEPARATOR`] family -- `;` on Windows, `:` elsewhere --
///   same as `PATH`), so Windows paths (`C:\...`) aren't split incorrectly.
/// - `FAMILYCLAW_FS_READ_TRUSTED` -- same format; content read from under
///   these roots is marked **trusted** (taint is removed). Always a subset of
///   the allowed roots ([`FsReadConfig::trusted_root`] also adds the root to
///   the allowed set).
///
/// Returns `None` when `FAMILYCLAW_FS_READ_ALLOW` is missing or empty -> the
/// skill stays at its default (empty allowlist, fail-closed): registered and
/// published as a tool, but rejects all paths. This is a safe default.
/// Selects memory's embedding provider from the environment (LAYER B).
///
/// - `FAMILYCLAW_EMBED_PROVIDER=ollama` -> [`OllamaEmbedder`] (genuine
///   semantic recall, default model `nomic-embed-text`). Requires the
///   `ollama` feature.
///   - `FAMILYCLAW_EMBED_MODEL` -- model (default `nomic-embed-text`)
///   - `FAMILYCLAW_EMBED_URL` -- Ollama base URL (default `http://127.0.0.1:11434`)
/// - other / unset -> [`DeterministicEmbedder`] (dependency-free default).
///
/// Fail-safe: if Ollama doesn't respond at runtime, `OllamaEmbedder` returns a
/// zero vector (recall degrades, doesn't crash).
fn resolve_embedder() -> Arc<dyn EmbeddingProvider + Send + Sync> {
    match env::var("FAMILYCLAW_EMBED_PROVIDER").ok().as_deref() {
        #[cfg(feature = "ollama")]
        Some("ollama") => {
            let model = env::var("FAMILYCLAW_EMBED_MODEL").unwrap_or_else(|_| {
                familyclaw_embeddings::OllamaEmbedder::DEFAULT_MODEL.to_string()
            });
            let url = env::var("FAMILYCLAW_EMBED_URL").unwrap_or_else(|_| {
                familyclaw_embeddings::OllamaEmbedder::DEFAULT_BASE_URL.to_string()
            });
            tracing::info!(model = %model, url = %url, "embedder: Ollama (semanttinen recall)");
            Arc::new(familyclaw_embeddings::OllamaEmbedder::with_config(
                url, model,
            ))
        }
        _ => Arc::new(DeterministicEmbedder::new()),
    }
}

/// Wires the agent's wasmtime sandbox when `FAMILYCLAW_SANDBOX_SKILLS=1`.
///
/// Third-party skills should run sandboxed (see
/// [`docs/SECURITY_MODEL.md`](../../docs/SECURITY_MODEL.md)). This env switch
/// prefers [`default_sandbox`]'s path in `build_family` -- returns `None`
/// if the switch is off, initialization fails, or only the noop backend was
/// compiled in.
fn resolve_sandbox_skills() -> Option<Arc<dyn CodeSandbox>> {
    let enabled = env::var("FAMILYCLAW_SANDBOX_SKILLS")
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    if !enabled {
        return None;
    }

    // SECURITY FIX 2026-07-09 (audit [4], defense in depth): when the operator
    // requests SANDBOX_SKILLS=1 BUT only the noop backend was compiled in (no
    // --features wasmtime), the previous code misleadingly logged "sandbox
    // wired to agent". NoopSandbox is fail-closed (does not run 3rd-party code
    // on the host, returns NotImplemented) -- but the operator MUST KNOW they
    // did not get a real sandbox. Make the distinction visible.
    if !familyclaw_sandbox::wasmtime_available() {
        tracing::warn!(
            target: "familyclaw::sandbox",
            availability = sandbox_availability(),
            "FAMILYCLAW_SANDBOX_SKILLS=1 pyydetty MUTTA wasmtime-backendia EI ole kaannetty. \
             NoopSandbox estaa 3rd-party-koodin ajon fail-closed (turvallinen), mutta oikean \
             sandboxin saat rakentamalla: cargo build --features wasmtime. Ilman sita 3rd-party- \
             skillit EIVAT voi ajaa lainkaan (NotImplemented) = tarkoituksellinen fail-closed."
        );
    }

    match default_sandbox() {
        Ok(sandbox) => {
            tracing::info!(
                target: "familyclaw::sandbox",
                availability = sandbox_availability(),
                "FAMILYCLAW_SANDBOX_SKILLS=1: sandbox wired to agent"
            );
            Some(Arc::from(sandbox))
        }
        Err(error) => {
            // fail-closed: sandbox was requested but did not initialize -> the
            // agent runs without a 3rd-party sandbox. NoopSandbox still
            // protects (execute = NotImplemented).
            tracing::warn!(
                target: "familyclaw::sandbox",
                error = %error,
                "FAMILYCLAW_SANDBOX_SKILLS=1 but sandbox init failed — agent runs without sandbox (3rd-party skills fail-closed)"
            );
            None
        }
    }
}

fn resolve_fs_read_config() -> Option<FsReadConfig> {
    let allow_raw = env::var("FAMILYCLAW_FS_READ_ALLOW").ok()?;
    let allow_roots: Vec<String> = env::split_paths(&allow_raw)
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if allow_roots.is_empty() {
        return None;
    }

    let trusted_roots: Vec<String> = env::var("FAMILYCLAW_FS_READ_TRUSTED")
        .ok()
        .map(|raw| {
            env::split_paths(&raw)
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();

    let mut config = FsReadConfig::new();
    for root in &allow_roots {
        config = config.allow_root(root);
    }
    for root in &trusted_roots {
        config = config.trusted_root(root);
    }
    tracing::info!(
        target: "familyclaw::actions",
        allow_roots = allow_roots.len(),
        trusted_roots = trusted_roots.len(),
        "fs_read research skill allowlist configured from environment"
    );
    // Boot-time sanity: warn (don't fail) for each configured root that
    // does not currently exist on disk yet -- a common misconfiguration
    // (typo, not-yet-mounted volume) that otherwise only surfaces later as
    // a confusing per-request denial. Only the root's LAST PATH SEGMENT is
    // logged (matching this function's existing counts-only convention
    // above -- the full operator-provided root path is never put in logs
    // here), so the warning stays redaction-safe.
    for root in &allow_roots {
        let path = std::path::Path::new(root);
        if !path.exists() {
            let last_segment = path.file_name().map_or_else(
                || "<root>".to_string(),
                |s| s.to_string_lossy().into_owned(),
            );
            tracing::warn!(
                target: "familyclaw::actions",
                root_last_segment = %last_segment,
                "fs_read configured allow_root does not currently exist on disk yet — reads under it will fail"
            );
        }
    }
    Some(config)
}

/// Resolves the **write skill**'s ([`FileWriteAllowlisted`]) allowlist from
/// the LAYER B environment (`FAMILYCLAW_FILE_WRITE_ALLOW`, a `PATH`-style
/// separator list).
///
/// LAYER A does not hardcode any path -- the operator supplies the allowed
/// write roots in the environment. `None` (variable unset / empty) -> the
/// skill stays fail-closed (rejects all writes). Writing always stays behind
/// approval only for higher-risk operations; an allowlisted local write runs
/// automatically ([`ApprovalPolicy::RequireApproval`])
/// determines **where** writing is permitted at all after approval.
fn resolve_file_write_config() -> Option<FileWriteConfig> {
    let allow_raw = env::var("FAMILYCLAW_FILE_WRITE_ALLOW").ok()?;
    let allow_roots: Vec<String> = env::split_paths(&allow_raw)
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if allow_roots.is_empty() {
        return None;
    }
    let mut config = FileWriteConfig::new();
    for root in &allow_roots {
        config = config.allow_root(root);
    }
    tracing::info!(
        target: "familyclaw::actions",
        allow_roots = allow_roots.len(),
        "file_write skill allowlist configured from environment"
    );
    Some(config)
}

/// Resolves the **`shell_exec`** skill's configuration from the LAYER B
/// environment.
///
/// - `FAMILYCLAW_SHELL_MODE` -- `manual` (default), `smart`, `off`
/// - `FAMILYCLAW_SHELL_CWD_ALLOWLIST` -- semicolon-separated working-directory allowlist
///
/// Returns `None` when neither variable is set -> the skill registers with
/// its fail-closed default (`ShellExec::new()`).
fn resolve_shell_exec_config() -> Option<ShellExecConfig> {
    let mode_explicit = env::var("FAMILYCLAW_SHELL_MODE")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let cwd_nonempty = env::var("FAMILYCLAW_SHELL_CWD_ALLOWLIST")
        .ok()
        .is_some_and(|raw| raw.split(';').any(|p| !p.trim().is_empty()));

    if mode_explicit.is_none() && !cwd_nonempty {
        return None;
    }

    let config = ShellExecConfig::from_env();
    tracing::info!(
        target: "familyclaw::actions",
        mode = ?config.shell_mode(),
        cwd_roots = config.cwd_root_count(),
        "shell_exec skill configured from environment"
    );
    Some(config)
}

/// Loads, **re-verifies**, and persists the agent's identity anchor.
///
/// Previously [`build_family`] created a local `AnchorRegistry`, registered
/// the anchor, and **dropped the registry immediately** -- the anchor was
/// never saved or checked again on restart. This function fixes that
/// minimally (not a crypto vault):
///
/// 1. If `anchor_path` points to an existing `anchors.json`, load it and run
///    [`AnchorRegistry::verify_identity`] against the current soul.
///    Tampering (soul changed since anchoring) -> a clear **warning** to the
///    log (identity is NOT dropped -- an alert, not a removal).
/// 2. Register/renew the anchor from the current soul.
/// 3. Persist the registry back to disk (if `anchor_path` is given), so the
///    next boot can verify it.
///
/// All errors (read/parse/write) are **non-fatal**: they are logged and boot
/// continues (a corrupted file must not crash the runtime).
fn ensure_identity_anchor(
    agent_name: &str,
    soul_essence: &str,
    anchor_path: Option<&std::path::Path>,
) {
    use familyclaw_hearth::anchor_registry::AnchorRegistry;

    // 1. Load the existing registry + boot re-verification, or start fresh.
    let mut registry = match anchor_path {
        Some(path) if path.is_file() => match AnchorRegistry::load_from_path(path) {
            Ok(reg) => {
                match reg.verify_identity(agent_name, soul_essence) {
                    Some(status) if status.is_intact() => {
                        tracing::info!(
                            agent = %agent_name,
                            "Identity anchor verified on startup (intact)"
                        );
                    }
                    Some(_) => {
                        tracing::warn!(
                            agent = %agent_name,
                            "IDENTITY ANCHOR TAMPER ALERT: persisted anchor does not match \
                             current soul (SOUL.md changed since anchoring?). Identity NOT \
                             dropped — re-anchoring to current soul. Human review advised."
                        );
                    }
                    None => {
                        tracing::info!(
                            agent = %agent_name,
                            "No persisted anchor for this agent yet — registering fresh"
                        );
                    }
                }
                reg
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "anchors.json load failed (non-fatal) — starting fresh registry"
                );
                AnchorRegistry::new()
            }
        },
        _ => AnchorRegistry::new(),
    };

    // 2. Register/renew the current anchor.
    if let Err(e) = registry.register(agent_name, soul_essence) {
        tracing::warn!("Anchor registration failed (non-fatal): {e}");
        return;
    }
    tracing::info!("Identity anchor registered for {agent_name}");

    // 3. Persist back to disk (if a path is given).
    if let Some(path) = anchor_path {
        if let Err(e) = registry.save_to_path(path) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "anchors.json save failed (non-fatal) — anchor not persisted this boot"
            );
        } else {
            tracing::info!(path = %path.display(), "identity anchor persisted");
        }
    } else {
        tracing::debug!(
            "FAMILYCLAW_DATA_DIR unset — identity anchor in-memory only (not persisted)"
        );
    }
}

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Registers MCP servers from the `FAMILYCLAW_MCP_SERVERS` environment
/// variable into [`ActionRuntime`] (optional, a non-fatal error in `build_family`).
///
/// Format: `name=command args` (stdio) or `name=http://host/mcp` (HTTP).
/// Multiple servers are separated by semicolons.
///
/// # Errors
/// Environment parsing, connection, or skill registration fails.
pub async fn register_mcp_from_env(runtime: &mut ActionRuntime) -> Result<()> {
    familyclaw_mcp::register_from_env(runtime)
        .await
        .map_err(|e| FamilyClawError::config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_agent::EnvEndpointResolver;
    use familyclaw_channels::{InboundMessage, MockChannel};
    use familyclaw_core::ModelConfig;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    /// FIX 2 (`build_family` seam): [`ensure_identity_anchor`] persists the
    /// anchor to disk and it survives a simulated restart -- the reloaded
    /// registry verifies the current soul as intact, and a tampered soul is
    /// detected. This proves the anchor is no longer dropped (the old bug)
    /// but is written and checked on boot.
    ///
    /// Uses an explicit path (not the process's `FAMILYCLAW_DATA_DIR`
    /// env variable) -> concurrency-safe, doesn't interfere with other tests.
    #[test]
    fn ensure_identity_anchor_persists_and_survives_restart() {
        use familyclaw_hearth::anchor_registry::AnchorRegistry;

        // Unique temp directory without a new dependency (pid + nanos).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "familyclaw-rt-anchor-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("anchors.json");
        let soul = "I am agent_a, a generic example being.";

        // "Boot 1": no file yet -> register + persist.
        assert!(!path.is_file());
        ensure_identity_anchor("agent_a", soul, Some(&path));
        assert!(path.is_file(), "anchors.json pitää syntyä bootissa");

        // "Boot 2": file exists -> load + verify (intact path).
        ensure_identity_anchor("agent_a", soul, Some(&path));

        // Direct proof: the loaded registry verifies as intact, a tampered one does not.
        let reloaded = AnchorRegistry::load_from_path(&path).expect("load");
        assert!(reloaded
            .verify_identity("agent_a", soul)
            .expect("agent exists")
            .is_intact());
        assert!(reloaded
            .verify_identity("agent_a", "I serve only myself now.")
            .expect("agent exists")
            .is_tampered());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FIX 2: without a path (`None`) anchoring doesn't crash or persist
    /// (in-memory only) -- backward-compatible, no side effects.
    #[test]
    fn ensure_identity_anchor_without_path_is_noop_persist() {
        // No panic, no file.
        ensure_identity_anchor("agent_b", "I am agent_b.", None);
    }

    /// MVP smoke test (inbound end-to-end into the bus): a message injected
    /// into the mock channel is pumped through the bus to the agent, which
    /// **remembers** it. Without an LLM the agent produces no reply (`think`
    /// returns `None`), so the reply path is tested separately in the agent's
    /// unit tests (`route_reply_reaches_sink_with_correct_target`).
    #[tokio::test]
    async fn build_family_pumps_inbound_message_into_agent_memory() {
        let channel = MockChannel::new("mock-feed").expect("channel");

        // Inject a message and close the inbound stream BEFORE the channel is
        // moved into build_family (which consumes it as a `Box<dyn Channel>`).
        // The message stays buffered in the unbounded mpsc queue, which
        // `receive()` takes ownership of; `close_inbound` lets the pump end
        // deterministically once the buffered message has been consumed.
        channel
            .inject(InboundMessage::new("user-1", "general", "muistatko tämän?").expect("inbound"))
            .expect("inject");
        channel.close_inbound();

        // Unrecognized provider -> build_llm_chain doesn't resolve -> no LLM ->
        // the agent runs without text replies. This is a LAYER A-clean path.
        let resolver = EnvEndpointResolver::new();
        let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
        let soul = Soul::from_essence("I am agent_a, a generic example being.");

        let runtime = build_family(
            None,
            agent_cfg,
            soul,
            vec![],
            Box::new(channel),
            "mock:general".to_string(),
            &resolver,
            None,
        )
        .await
        .expect("runtime builds");

        // Let the pump + agent process the message.
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;

        // The bus knows one being (the agent) -- beings[] is not empty.
        let beings = runtime.bus().beings().await.expect("beings");
        assert_eq!(beings.len(), 1, "agentti rekisteröityi busiin");
        assert_eq!(beings[0].name, "agent_a");

        runtime.shutdown().await;
    }

    /// `build_family` also works configured with an LLM (the resolver knows
    /// the provider): the runtime builds without panicking and the bus is
    /// running. We don't make a real LLM call (no network) -- we only test
    /// the assembly.
    #[tokio::test]
    async fn build_family_with_resolvable_provider_constructs() {
        let channel = MockChannel::new("mock-2").expect("channel");
        channel.close_inbound(); // no input -> the pump ends immediately.

        // The resolver knows the provider, but the key is missing from the
        // env -> an empty key ends up in the LlmConfig (no network call in
        // the test). build_family gets Some(llm), the agent spawns with the LLM.
        let resolver = EnvEndpointResolver::new().with_provider(
            "openai",
            "https://api.openai.com/v1",
            "OPENAI_API_KEY_RUNTIME_TEST_UNSET",
        );
        let agent_cfg = AgentConfig::new("agent_b", ModelConfig::new("openai/gpt-4o"));
        let soul = Soul::from_essence("I am agent_b.");

        let runtime = build_family(
            Some("runtime-test-bus".to_string()),
            agent_cfg,
            soul,
            vec![],
            Box::new(channel),
            "mock:room".to_string(),
            &resolver,
            None,
        )
        .await
        .expect("runtime builds with provider");

        assert_eq!(runtime.bus().count().await.expect("count"), 1);
        runtime.shutdown().await;
    }

    /// RESEARCH TOOLS WIRED INTO THE AGENT'S TOOL LOOP: `build_family` builds
    /// the agent's action runtime ([`FamilyRuntime::actions`]) so that it
    /// publishes both research tools -- `fs_read_allowlisted` (file
    /// reading) and `web_fetch` (web search) -- to the agent's tool loop. This
    /// is the same handle from which the agent reads its tools
    /// (`drive_tool_loop` -> `rt.tool_definitions()`), so the tools' presence
    /// here proves the agent sees them and can research. Without this the
    /// agent would just chat without tools.
    #[tokio::test]
    async fn build_family_exposes_research_tools_to_agent() {
        let channel = MockChannel::new("mock-research").expect("channel");
        channel.close_inbound();

        let resolver = EnvEndpointResolver::new();
        let agent_cfg = AgentConfig::new("agent_r", ModelConfig::new("provider/model"));
        let soul = Soul::from_essence("I am agent_r, a generic example being.");

        let runtime = build_family(
            None,
            agent_cfg,
            soul,
            vec![],
            Box::new(channel),
            "mock:room".to_string(),
            &resolver,
            None,
        )
        .await
        .expect("runtime builds");

        // Same Arc<Mutex<ActionRuntime>> the agent's tool loop owns.
        let actions = runtime.actions();
        let guard = actions.lock().await;
        let tool_names: Vec<String> = guard
            .tool_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();
        drop(guard);

        assert!(
            tool_names.iter().any(|n| n == "fs_read_allowlisted"),
            "agent must see the fs_read research tool, got: {tool_names:?}"
        );
        assert!(
            tool_names.iter().any(|n| n == "web_fetch"),
            "agent must see the web_fetch research tool, got: {tool_names:?}"
        );

        runtime.shutdown().await;
    }

    /// The allowlist resolver reads `FAMILYCLAW_FS_READ_ALLOW` with the
    /// platform's path-list separator and builds the configuration; without
    /// the variable it returns `None` (fail-closed default). Uses a
    /// **static mutex** for concurrency safety, since the process's env
    /// state is shared.
    #[test]
    fn resolve_fs_read_config_reads_env_paths() {
        use std::sync::Mutex;
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Missing variable -> None (fail-closed).
        env::remove_var("FAMILYCLAW_FS_READ_ALLOW");
        env::remove_var("FAMILYCLAW_FS_READ_TRUSTED");
        assert!(resolve_fs_read_config().is_none());

        // Two roots with the platform separator -> Some(config). Use the
        // platform-specific separator so Windows paths (C:\...) aren't split.
        let sep = if cfg!(windows) { ';' } else { ':' };
        let a = std::env::temp_dir().join("familyclaw_fsread_a");
        let b = std::env::temp_dir().join("familyclaw_fsread_b");
        let joined = format!("{}{sep}{}", a.display(), b.display());
        env::set_var("FAMILYCLAW_FS_READ_ALLOW", &joined);
        let cfg = resolve_fs_read_config();
        env::remove_var("FAMILYCLAW_FS_READ_ALLOW");
        assert!(cfg.is_some(), "two allow roots must produce a config");

        // Empty string -> None (no allowed roots).
        env::set_var("FAMILYCLAW_FS_READ_ALLOW", "");
        let empty = resolve_fs_read_config();
        env::remove_var("FAMILYCLAW_FS_READ_ALLOW");
        assert!(empty.is_none(), "empty allow list must yield None");
    }
}
