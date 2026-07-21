//! Agent runtime — assembles everything into a single being (design §2 layer 2).
//!
//! [`Agent`] owns the being's entire runtime state:
//! - [`AgentConfig`] — identity + model configuration (`familyclaw-core`),
//! - [`Soul`] — loaded profile ([`crate::soul`]),
//! - [`EmotionState`] — 19-dim emotion state (`familyclaw-emotion`),
//! - [`MemoryStore`] handle — Eternal Thread (`familyclaw-memory`),
//! - [`DurableContext`] — crash-resistant step log (`familyclaw-durable`),
//! - [`BusHandle`] + [`BeingId`] — Resonance Bus connection (`familyclaw-bus`).
//!
//! [`AgentActor`] wraps [`Agent`] into a Ractor actor that joins the bus,
//! handles [`BusMessage`]s, updates the emotion state (affective contagion),
//! records memories, and publishes emotion pulses to its siblings.
//!
//! ## OSS boundary (Layer A)
//! This module does not hardcode family members' souls, model names, keys,
//! or paths. Everything is loaded at runtime from configuration and the
//! profile directory. Examples use generic names.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;

use familyclaw_actions::{
    ActionId, ActionRuntime, ActionTaskId, ApprovalId, AuditCollector, AuditKind, ExecAuditEvent,
    McpToolDescriptor,
};
use familyclaw_bus::{
    BeingId, BeingInfo, BusHandle, BusMessage, MessageOrigin, ResonanceMessage, TaskEventKind,
};
use familyclaw_channels::{OutboundKind, OutboundMessage};
use familyclaw_core::time::Timestamp;
use familyclaw_core::{time, AgentConfig, FamilyClawError, Result};
use familyclaw_durable::{DurableContext, Journal};
use familyclaw_emotion::{
    default_governing_profile, ActionDecision, Dimension, EmotionActionGoverning,
    EmotionActionGovernor, EmotionCalibration, EmotionState, GoverningProfile, NeutralCalibration,
};
use familyclaw_memory::{
    DecayPolicy, ImportanceFactors, Memory, MemoryStore, RetrievalContext, RetrievalResult,
};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::llm::{LlmConfig, LlmMessage, ToolCall, ToolDefinition};
use crate::llm_chain::LlmFailover;
use crate::resumable::{InMemoryResumableStore, ResumableTurn, ResumableTurnStore};
use crate::soul::Soul;
use crate::watchdog;
use familyclaw_sandbox::{CodeSandbox, SandboxOutput, SandboxRequest};

/// Type-erased memory store for trait-object-based agents.
pub type ErasedMemoryStore = Arc<dyn MemoryStore + Send + Sync>;

/// Reply channel (C1 Model A): the mpsc sender half that Agent uses to
/// push the LLM response out to the channel. **mpsc, NOT bus** — publishing
/// to the bus would trigger a new [`Agent::handle_turn`] (infinite loop).
///
/// The gateway owns the receiver half ([`new_reply_channel`]) and calls
/// `Channel::send`. Agent never calls the channel directly.
///
/// [`UnboundedSender::send`](tokio::sync::mpsc::UnboundedSender::send) is not
/// async and does not block — which is why it's safe to call from the
/// synchronous [`Agent::route_reply`].
pub type ReplySink = tokio::sync::mpsc::UnboundedSender<OutboundMessage>;

/// Lightweight observability event that the agent emits as a turn progresses.
///
/// Intentionally a **small, generic enum** — NO `MetricsRegistry` handle
/// on the agent (Layer A decoupling is preserved: the agent doesn't know
/// about the metrics stack, just like it doesn't know about channels behind
/// [`ReplySink`]). The runtime bridges these events to the observability bus
/// ([`crate::MetricEventSink`]). Variants are generic and never carry user
/// or Layer B data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricEvent {
    /// One fresh (non-replay) turn finished processing.
    TurnCompleted,
    /// One tool call was dispatched in the tool loop (fresh, non-replay).
    ToolDispatched,
}

/// Observability sink: where the agent pushes [`MetricEvent`]s.
///
/// **Bounded** [`tokio::sync::mpsc::Sender`] on purpose: if the receiver
/// (runtime bridge) falls behind, overflow is **dropped**
/// (`try_send` returns an error which the emit call ignores) —
/// observability must NOT grow the queue unboundedly on the hot path nor
/// block the agent's turn. Metrics are supplementary information; a few
/// dropped events during a high-load spike is acceptable, a memory leak is
/// not. (This is the `try_send`-drop pattern, safer than an unbounded
/// channel when there are many emitters, e.g. parallel multi-agent runs.)
pub type MetricEventSink = tokio::sync::mpsc::Sender<MetricEvent>;

/// Observability sink capacity (bounded channel size). Large enough for
/// normal tick/turn volume, small enough that memory doesn't grow
/// uncontrollably if the consumer stalls.
pub const METRIC_SINK_CAPACITY: usize = 1024;

/// Builds a reply channel pair: [`ReplySink`] for the agent + the receiver
/// half for the gateway (C1 Model A — the gateway owns the recv half and
/// calls `Channel::send`).
#[must_use]
pub fn new_reply_channel() -> (
    ReplySink,
    tokio::sync::mpsc::UnboundedReceiver<OutboundMessage>,
) {
    tokio::sync::mpsc::unbounded_channel()
}

/// Type-erased journal for trait-object-based agents.
pub type ErasedJournal = Arc<dyn Journal + Send + Sync>;

/// The outcome of a single turn, recorded to the durable log
/// deterministically. Kept small and serializable so replay is
/// lightweight and doesn't depend on external state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TurnOutcome {
    /// The turn's sequence number (0-based) in this agent's lifecycle.
    pub turn: u64,
    /// Whether the turn was saved as a memory.
    pub remembered: bool,
    /// A short, human-readable summary of what happened in the turn.
    pub summary: String,
}

/// How much a sibling's emotion pulse "attaches" to the recipient
/// (affective contagion coefficient, design §2.2). Generic baseline default —
/// per-instance calibration (Layer B) may tune this later.
const CONTAGION_FACTOR: f32 = 0.25;

/// Default TTL in minutes for a resumable turn, when the pending approval's
/// expiry moment cannot be obtained from [`ActionRuntime`] for some reason
/// (e.g. the approval has already been evicted). Kept equal to the actions
/// layer's `DEFAULT_APPROVAL_TTL_MINUTES` so a resumable turn never outlives
/// the approval. In practice the expiry is derived directly from the pending
/// approval ([`ActionRuntime::pending_expiry_for`]); this is used only as a
/// fallback.
const RESUMABLE_DEFAULT_TTL_MINUTES: i64 = 60;

/// After each turn, the emotion state recovers toward neutral by this
/// percentage. A value of 0.10 (10%) means: after 10 turns of continuous
/// sibling influence, the emotion state is less than half of its maximum
/// (exponential decay). This prevents feedback-loop saturation.
const HOMEOSTASIS_RATE: f32 = 0.10;

/// How many CONSECUTIVE history messages (user+assistant) are kept per
/// conversation for LLM context. 20 = ~10 turn pairs: enough for continuity
/// of a Discord conversation without bloating the context. The oldest is
/// dropped once the cap is exceeded (sliding window).
const HISTORY_MAX_MESSAGES: usize = 20;

/// Character cap for a single history message. A long message is truncated
/// to this before being saved to history — prevents one giant message from
/// consuming the entire window. Default; overridable via
/// `FAMILYCLAW_HISTORY_MAX_CHARS` (see [`history_max_chars_per_msg`]) — a
/// low default here stunts the agent's memory of its own longer replies once
/// [`crate::llm::DEFAULT_MAX_TOKENS`] is raised, so deployments generating
/// long replies routinely can raise this too.
const HISTORY_MAX_CHARS_PER_MSG: usize = 1500;

/// Minimum accepted value for `FAMILYCLAW_HISTORY_MAX_CHARS` — guards against
/// a misconfigured tiny value truncating history into uselessness.
const HISTORY_MAX_CHARS_MIN: usize = 200;

/// Reads the `FAMILYCLAW_HISTORY_MAX_CHARS` environment variable, or returns
/// [`HISTORY_MAX_CHARS_PER_MSG`]. Follows the same env-var-reader shape as
/// [`crate::watchdog::turn_watchdog_secs`]: parse, filter to a valid range,
/// default on anything else (missing, unparseable, or below
/// [`HISTORY_MAX_CHARS_MIN`]).
#[must_use]
fn history_max_chars_per_msg() -> usize {
    std::env::var("FAMILYCLAW_HISTORY_MAX_CHARS")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|&n: &usize| n >= HISTORY_MAX_CHARS_MIN)
        .unwrap_or(HISTORY_MAX_CHARS_PER_MSG)
}

/// Memory layer tag for a user's chat message (session-scoped hydration).
const CHAT_USER_TAG: &str = "chat:user";

/// Memory layer tag for the agent's chat reply (session-scoped hydration).
const CHAT_ASSISTANT_TAG: &str = "chat:assistant";

/// `Memory::source` for chat history entries (distinguishes them from bus memories).
const CHAT_HISTORY_SOURCE: &str = "chat_history";

/// How many memories tagged with a chat role tag are fetched on cold start
/// (in-process RAM history empty → hydrate from store).
const HISTORY_HYDRATE_LIMIT: usize = 20;

/// Tool loop (Phase 1 keystone) configuration.
///
/// Limits how many times [`Agent::think`] may cycle
/// (LLM call → tool call → result back → new LLM call) before the
/// loop is force-stopped. This is a **safety limit**, not a target: a
/// well-behaved model stops on its own once it stops requesting tools
/// (see [`Agent::think`]). The limit guarantees that a misbehaving or
/// looping model does not get stuck in an infinite cycle nor burn the
/// budget indefinitely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolLoopConfig {
    /// Maximum allowed number of rounds (LLM calls) during a single turn.
    /// Every tool call — even an unknown one — consumes one round,
    /// so the limit bounds the loop even if the model only requests
    /// invalid tools. Default [`ToolLoopConfig::DEFAULT_MAX_ITERATIONS`].
    pub max_iterations: u32,
}

impl ToolLoopConfig {
    /// Default round limit: eight LLM calls per turn. Enough for a typical
    /// multi-step tool sequence without leaving the loop unbounded.
    pub const DEFAULT_MAX_ITERATIONS: u32 = 8;
}

impl Default for ToolLoopConfig {
    /// Default: [`ToolLoopConfig::DEFAULT_MAX_ITERATIONS`] rounds.
    fn default() -> Self {
        Self {
            max_iterations: Self::DEFAULT_MAX_ITERATIONS,
        }
    }
}

/// The tool loop's **internal outcome** (Phase 1 keystone).
///
/// This is [`Agent::run_tool_loop`]'s own control type: it separates the
/// loop's three possible termination modes from each other, typed. It is
/// intentionally `enum`-private (not `pub`) — it is the loop's *mechanism*,
/// not the agent's *public contract*. The public contract is
/// [`ThinkOutcome`], which [`think`] converts this into:
///
/// | `ToolLoopOutcome`         | → | [`ThinkOutcome`]                 |
/// |---------------------------|---|----------------------------------|
/// | [`Answer`](Self::Answer)  | → | [`Reply`](ThinkOutcome::Reply)   |
/// | [`AwaitingApproval`](Self::AwaitingApproval) | → | [`Suspended`](ThinkOutcome::Suspended) |
/// | [`MaxIterations`](Self::MaxIterations) | → | [`NoReply`](ThinkOutcome::NoReply) |
///
/// Only `Answer` → `Reply` is allowed to cross the user boundary (reply
/// channel + durable summary). `AwaitingApproval` and `MaxIterations` are
/// **non-reply control states**: their internal strings (including the raw
/// `approval_id`) are never routed to the end user. This separation fixes
/// the Phase 1 (1B) gap, where intermediate tokens leaked verbatim through
/// the reply pipe.
///
/// [`think`]: Agent::think
///
/// `PartialEq` (not `Eq`): `AwaitingApproval` carries a message stack
/// ([`LlmMessage`], `PartialEq` only) and raw arguments
/// (`serde_json::Value`, `PartialEq` only).
#[derive(Debug, Clone, PartialEq)]
enum ToolLoopOutcome {
    /// The model stopped at a final answer → route to the user.
    Answer(String),
    /// A tool requires human approval → execution is left waiting. Internal
    /// control state: the approval identifier ([`ApprovalId`]) lives in
    /// [`ActionRuntime`] for a later operator `approve` call —
    /// it is NEVER sent to the user. Converted into
    /// [`ThinkOutcome::Suspended`] in [`think`](Agent::think).
    ///
    /// **Resume bridge (roadmap §6):** this variant also carries the state
    /// that resuming needs — the message stack, the identifier, name, and
    /// arguments of the tool call that was interrupted. [`think`](Agent::think)
    /// saves a secret-free [`ResumableTurn`] from these to the durable layer
    /// under the key `approval_id` before it returns
    /// [`ThinkOutcome::Suspended`].
    AwaitingApproval {
        /// Name of the tool that required approval (log/diagnostics + resume).
        tool: String,
        /// The granted approval's **typed** identifier. The operator
        /// (or the resume path) uses this to continue execution of the
        /// suspended task.
        approval_id: ApprovalId,
        /// An operator-safe, redacted summary of what the approval concerns
        /// (skill name + identifiers). **No secrets, no raw payload** —
        /// derived from the pending record's redacted summary
        /// ([`ActionRuntime::pending_summary_for`]).
        redacted_summary: String,
        /// The tool loop's message stack at the moment of suspension
        /// (system + user + assistant/tool messages so far). Resume
        /// continues from this.
        messages: Vec<LlmMessage>,
        /// The LLM identifier of the tool call that was interrupted (the
        /// future `tool_result` will bind to this when resuming).
        tool_call_id: String,
        /// The raw arguments of the tool call that was interrupted. **Only
        /// used to hash the arguments** ([`ResumableTurn::new`] computes a
        /// SHA-256 digest from them and does not store the value itself) —
        /// never raw to disk nor to the user.
        arguments: serde_json::Value,
    },
    /// The round limit was reached without a final answer → no user reply.
    MaxIterations {
        /// The round limit reached (log/diagnostics).
        iterations: u32,
    },
}

/// [`Agent::think`]'s **public outcome** (1C, roadmap amendment 3).
///
/// > **Suspend is a STATE, not a string.** This enum makes it
/// > first-class: three mutually exclusive outcomes, of which only
/// > one ([`Reply`](Self::Reply)) is meant for the end user.
///
/// This replaces the earlier `Option<Result<String>>` return, where two
/// different meanings — "no reply this turn" and "reply = `text`" — were
/// packed into `None`/`Some(Ok(text))`, and suspend had to be **silenced
/// into `None`**. However `None` did not carry the suspend state, so resume
/// (a later `approve`) lost context. Now suspend is its own variant,
/// carrying exactly the information resume needs — and never leaking into
/// the reply pipe (that was the 1B leak).
///
/// ## User-boundary invariant
/// - [`Reply`](Self::Reply) → routed to the user (reply channel) and
///   attached to the turn's durable summary.
/// - [`Suspended`](Self::Suspended) → the turn is suspended pending approval;
///   recorded to durable state (id + redacted summary) and a short
///   notification is sent to the user (no automatic popup).
/// - [`NoReply`](Self::NoReply) → nothing is done (no text, no suspend).
///
/// The caller treats `NoReply` as silence; `Suspended` preserves the
/// resume state and notifies the user of the wait.
///
/// ## Secrecy invariant
/// [`Suspended::redacted_summary`](Self::Suspended) is an **operator-safe**
/// string: only the skill name and identifiers, no raw approval content,
/// no secrets, no Layer B data. It is derived from the pending record's
/// redacted summary ([`ActionRuntime::pending_summary_for`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThinkOutcome {
    /// The model produced a final text reply → route to the user.
    ///
    /// Arises from two paths: the single-shot path's ([`Agent::think`]
    /// without `actions`) text **and** the tool loop's `Answer` termination.
    Reply(String),
    /// A tool required human approval → the turn **suspended**
    /// pending approval. **NEVER** crosses into the reply pipe.
    ///
    /// Carries exactly the information resume needs: the typed approval
    /// identifier (with which `approve` continues execution) and an
    /// operator-safe redacted summary of what the approval concerns.
    Suspended {
        /// Identifier of the granted approval. Resume continues with this
        /// ([`ActionRuntime::approve`]).
        approval_id: ApprovalId,
        /// Redacted, operator-safe summary (no secrets, no raw payload).
        /// Kept in the turn's durable state for resume and operator display.
        redacted_summary: String,
    },
    /// No reply for this turn — no text and no suspend.
    ///
    /// Arises when: there is no LLM client (harmless no-op), the tool loop
    /// hit the round limit without producing text (`MaxIterations`), or the
    /// model produced no text. The caller routes nothing.
    NoReply,
}

/// Agent — a single being that assembles configuration, soul, emotion state,
/// memory, a crash-resistant log, and a bus connection.
///
/// Uses trait objects (`Box<dyn ...>`) instead of generics, so that
/// third-party developers can build on the platform without complex
/// type parameters. This is the required "burning down Generics Hell".
///
/// `Agent` is not itself an actor — it is the actor's *state*. Use the
/// [`Agent::spawn`] method to attach it to the bus as an actor.
pub struct Agent {
    /// Identity + model configuration.
    config: AgentConfig,
    /// The being's identifier used on the bus (derived from `config.id`).
    being_id: BeingId,
    /// The loaded soul (profile). [`Soul::default`] on the bare runtime.
    soul: Soul,
    /// Current emotion state (19-dim VAD).
    emotion: EmotionState,
    /// Memory substrate (Eternal Thread). Shared, so that multiple branches
    /// (actor + external reader) can use the same storage.
    memory: ErasedMemoryStore,
    /// Crash-resistant step log (deterministic replay).
    durable: DurableContext<ErasedJournal>,
    /// Resonance Bus handle (for publishing and querying).
    bus: BusHandle,
    /// How many turns have been processed (for sequencing durable step names).
    turn_counter: u64,
    /// Turn watchdog: whether a reply (Message/Progress) was sent to the user this turn.
    turn_user_reply_sent: AtomicBool,
    /// Turn watchdog: reply intentionally suppressed (governor Hesitate/Reflect, pulse).
    turn_reply_suppressed: AtomicBool,
    /// Active typing heartbeat; cancelled when the turn ends or the watchdog cuts it off.
    typing_abort: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
    /// Per-conversation short-term memory for LLM context (sliding window, at
    /// most [`HISTORY_MAX_MESSAGES`] messages per key). The key is built by
    /// [`Agent::conversation_key`] from the message's origin (`channel_id` +
    /// `conversation`). This fixes the "agent replies only once" problem:
    /// without this, every turn was built from scratch as `[system, user]`
    /// without prior turns, so the agent never saw conversation continuity.
    ///
    /// **Replay safety:** history is appended to ONLY on a fresh turn
    /// (`!self.durable.is_replaying()`), so deterministic replay doesn't
    /// double-record messages. History is in-process (not journaled):
    /// after a crash it is rebuilt from future turns, which is acceptable
    /// for short-term memory (long-term memory lives in [`Agent::memory`]).
    history: HashMap<String, VecDeque<LlmMessage>>,
    /// LLM failover chain for thinking (optional, so tests work without an
    /// LLM). [`Agent::new`] wraps a single [`LlmConfig`] into a 1-length
    /// chain ([`LlmFailover::single`]); the full fallback chain is wired via
    /// [`Agent::with_failover`] (e.g. the runtime's `build_family`). This
    /// gives [`Agent::think`] failover: if the primary dies (timeout/HTTP/rate),
    /// the chain's next client is tried until one succeeds.
    llm: Option<LlmFailover>,
    /// Sandbox for executing code (optional, with the `wasmtime` feature).
    sandbox: Option<Arc<dyn CodeSandbox>>,
    /// Reply channel (C1 Model A): where the LLM response is pushed out. `None` =
    /// drop replies (current, backward-compatible behavior).
    reply_sink: Option<ReplySink>,
    /// Observability sink (Phase 2): where the agent pushes [`MetricEvent`]s
    /// (turn completed, tool call). `None` = no metrics (default,
    /// backward-compatible). The runtime bridges this to the metrics stack.
    metrics_sink: Option<MetricEventSink>,
    /// Reply target: the channel-specific reply address (conversation/channel id) to
    /// which [`Agent::route_reply`] sends. `None` = no known target
    /// (replies dropped even if a sink is installed).
    ///
    /// **Note (C2 gap):** since [`BusMessage`] currently does not carry
    /// channel origin (`MessageOrigin`), the reply target is given to the agent
    /// separately ([`Agent::with_reply_target`]). Once the broader C2 origin
    /// contract (origin field in the bus message) is built, the target can be
    /// derived from the per-message being handled. See open question.
    reply_target: Option<String>,
    /// Session isolation origin (F4). `None` = current shared-scope behavior
    /// (backward-compatible: all turns share the same memory scope).
    /// `Some(origin)` → the turn's memories are tagged with
    /// [`MessageOrigin::session_tag`], and [`Agent::think`]'s recall is filtered
    /// with the same tag → memories from other sessions don't leak into
    /// each other's context (one agent + one memory store, scoped by tag — not
    /// per-session instances).
    ///
    /// **F2 dependency:** once [`ResonanceMessage`] carries origin per-message
    /// (F2 contract), this is set per-turn from the message being handled
    /// instead of statically at build time. Until then, origin is given via
    /// [`Agent::with_session`] (correct for a single session/agent).
    session: Option<crate::session::MessageOrigin>,
    /// Emotion -> action decision-maker (Phase 1 emotion governor). Default
    /// `None` → the agent behaves the old way (thinks on all messages,
    /// doesn't filter `EmotionPulse` out of the LLM). Layer B
    /// installs a per-being profile via [`Agent::with_governor_profile`].
    ///
    /// **Phase 1 task:** This field + the following filters
    /// (in `handle_turn`) + `EmotionActionGovernor` turn
    /// `EmotionPulse` signals into "blood" instead of LLM input, and
    /// decide which action mode (Hesitate / Reflect / Speak /
    /// `EngageWarmly` / `ReachOut` / Initiate) the agent uses.
    governor: Option<Box<dyn EmotionActionGoverning + Send + Sync>>,
    /// Emotion engine calibration (Layer B profile data, loaded at
    /// runtime from `calibration.json`). Default
    /// [`NeutralCalibration`] → emotion state pulls toward zero at a neutral
    /// decay rate (fully backward-compatible with the previous hardcoded
    /// behavior). When a non-neutral calibration is installed
    /// ([`Agent::with_calibration`]), it changes:
    /// - **homeostasis rest state** ([`Agent::apply_emotional_homeostasis`]):
    ///   emotion recovers toward the dimension's `baseline` value, not always zero;
    /// - **stimulus sensitivity** ([`Agent::apply_emotional_effect`]): a contact
    ///   stimulus is scaled by the dimension's `sensitivity` coefficient.
    ///
    /// Since the governor reads `self.emotion` state, calibration also affects
    /// the governor's [`ActionDecision`]s indirectly (different state → different decision).
    calibration: Box<dyn EmotionCalibration + Send + Sync>,
    /// Action runtime for the tool loop (Phase 1 keystone). Default
    /// `None` → [`Agent::think`] preserves the old **single-shot**
    /// behavior (one LLM call, no tools). When
    /// [`with_actions`](Agent::with_actions) installs an
    /// [`ActionRuntime`], `think()` runs the tool loop: builds
    /// tool definitions from the runtime's published MCP descriptors, gives them
    /// to the LLM, and routes the model's chosen tool calls back to the
    /// runtime until the model stops requesting tools (or the limit is reached).
    ///
    /// Interior mutability ([`Mutex`]): [`ActionRuntime::submit_task`] is
    /// `&mut self`, but `think()` borrows `&self` (a Ractor actor shares state).
    /// `Arc<Mutex<…>>` lets multiple branches (actor + external call) safely
    /// share the same runtime across `.await` boundaries.
    actions: Option<Arc<Mutex<ActionRuntime>>>,
    /// Tool loop safety limit (round count per turn). Only in effect when
    /// [`actions`](Agent::actions) is installed; otherwise the single-shot path
    /// doesn't loop at all. Default [`ToolLoopConfig::default`].
    tool_loop: ToolLoopConfig,
    /// **Storage surface for resumable turns** (suspend/resume bridge, roadmap §6).
    ///
    /// When the tool loop suspends waiting for human approval, the agent
    /// saves the suspension state ([`ResumableTurn`]) here under the key
    /// `approval_id`, so [`Agent::resume_approved`] can load it later
    /// (even after a process restart, if the surface is a crash-resistant
    /// [`crate::resumable::JournalResumableStore`]) and continue execution
    /// from where it left off.
    ///
    /// Default is the in-memory [`InMemoryResumableStore`] (same
    /// backward-compatible behavior: suspend is saved, but doesn't survive
    /// a crash). The operator/runtime swaps in a crash-resistant surface via
    /// [`Agent::with_resumable_store`].
    resumable: Arc<dyn ResumableTurnStore>,
    /// **Turn audit collector** (TURN-AUDIT, roadmap §6 D6): an observable
    /// event chain of the tool loop's lifecycle — turn start, every
    /// tool call (skill name + **redacted** result), suspend
    /// (`approval_id` + redacted summary), resume, and `stop_reason`
    /// (`answered` / `max-iter` / `suspended`).
    ///
    /// Default `None` → no recording (backward-compatible, no
    /// performance impact). When a collector is installed
    /// ([`with_turn_audit`](Agent::with_turn_audit)), each turn gets its own
    /// correlation identifier ([`ActionId`]), by which its full trace can be
    /// retrieved ([`turn_audit_for`](Agent::turn_audit_for)).
    ///
    /// **Secrecy invariant:** the `detail` of recorded events is run through
    /// [`familyclaw_actions::redact_free_text`] before storage — raw
    /// payload, tool arguments, or secrets never end up in the
    /// audit trail (defense in depth, even if the source is already redacted).
    /// Uses [`AuditCollector`] (does not invent a new one) — the same
    /// thread-safe, append-only (tamper-evident) surface that the actions layer
    /// uses.
    turn_audit: Option<Arc<AuditCollector>>,
    /// **Emotion state observation handle** (optional introspection seam).
    ///
    /// When the agent is spawned as an [`AgentActor`], its `emotion` field lives
    /// inside the actor and is not readable from outside (unlike
    /// [`memory`](Self::memory), which is `Arc`-shared). This optional,
    /// shared `Arc<Mutex<EmotionState>>` is the **smallest safe seam** through
    /// which an external observer (e.g. an example or an integration test)
    /// can read the agent's emotion state after cross-bus emotion contagion —
    /// **without changing the actor's `Msg` type or the bus delivery path**.
    ///
    /// `handle_turn_with_origin` mirrors `self.emotion` into this at the end of
    /// every turn (with contagion + homeostasis applied). Default `None` → no
    /// mirroring, no performance impact (fully backward-compatible).
    /// Same standard pattern as [`familyclaw_bus::AffectiveState::emotion`].
    ///
    /// Uses `std::sync::Mutex` (not tokio's async mutex): mirroring is a short,
    /// synchronous observation write at the end of the turn — no lock held
    /// across an `.await` boundary. Same type as the bus layer's `SharedEmotion`.
    emotion_probe: Option<Arc<std::sync::Mutex<EmotionState>>>,
}

impl Agent {
    /// Builds an agent from finished parts.
    ///
    /// The emotion state starts neutral. `being_id` is derived from the
    /// config's agent id, so the bus and memory identities match.
    /// The LLM client is optional - if given, the agent can use the LLM
    /// for thinking (the think method). The sandbox is optional, for executing code.
    #[must_use]
    pub fn new(
        config: AgentConfig,
        soul: Soul,
        memory: ErasedMemoryStore,
        durable: DurableContext<ErasedJournal>,
        bus: BusHandle,
        llm_config: Option<LlmConfig>,
        sandbox: Option<Arc<dyn CodeSandbox>>,
    ) -> Self {
        let being_id = BeingId::from_agent_id(config.id);
        // A single `LlmConfig` is wrapped into a 1-length failover chain: the
        // same behavior as before (no fallbacks), but `think()` now goes
        // through the failover interface. The full chain is wired via [`with_failover`].
        let llm = llm_config.map(LlmFailover::single);
        Self {
            config,
            being_id,
            soul,
            emotion: EmotionState::neutral(),
            memory,
            durable,
            bus,
            turn_counter: 0,
            turn_user_reply_sent: AtomicBool::new(false),
            turn_reply_suppressed: AtomicBool::new(false),
            typing_abort: std::sync::Mutex::new(None),
            history: HashMap::new(),
            llm,
            sandbox,
            reply_sink: None,
            metrics_sink: None,
            reply_target: None,
            session: None,
            governor: None,
            calibration: Box::new(NeutralCalibration),
            actions: None,
            tool_loop: ToolLoopConfig::default(),
            resumable: Arc::new(InMemoryResumableStore::new()),
            turn_audit: None,
            emotion_probe: None,
        }
    }

    /// Install a reply sink (C1 Model A). `None` = drop replies (current
    /// behavior, backward-compatible). Returns `self` for chaining,
    /// so the [`Agent::new`] signature stays unchanged (C1 requires: don't
    /// change the existing constructor).
    #[must_use]
    pub fn with_reply_sink(mut self, sink: ReplySink) -> Self {
        self.reply_sink = Some(sink);
        self
    }

    /// Install an **emotion state observation handle** (optional introspection seam).
    ///
    /// Gives an external observer a shared `Arc<Mutex<EmotionState>>`,
    /// into which the agent mirrors its final emotion state at the end of
    /// every turn. The sole purpose of this is to make the emotion state of a
    /// spawned ([`Agent::spawn`]) agent readable after cross-bus emotion
    /// contagion — for examples and integration tests. `None` (default) = no mirroring.
    ///
    /// Returns `self` for chaining ([`Agent::new`] signature unchanged).
    #[must_use]
    pub fn with_emotion_probe(mut self, probe: Arc<std::sync::Mutex<EmotionState>>) -> Self {
        self.emotion_probe = Some(probe);
        self
    }

    /// Install an observability sink (Phase 2). `None` (default) = no
    /// metrics. The runtime bridges these [`MetricEvent`]s to the shared
    /// `MetricsRegistry`. Returns `self` for chaining
    /// ([`Agent::new`] signature stays unchanged).
    #[must_use]
    pub fn with_metrics_sink(mut self, sink: MetricEventSink) -> Self {
        self.metrics_sink = Some(sink);
        self
    }

    /// Set the reply target (channel-specific reply address to which replies
    /// are routed). This is a temporary C2 bridge until [`BusMessage`] carries
    /// channel origin (`MessageOrigin`) per message. Returns `self`
    /// for chaining.
    #[must_use]
    pub fn with_reply_target(mut self, target: impl Into<String>) -> Self {
        self.reply_target = Some(target.into());
        self
    }

    /// **Resume as LIVE from atop durable replay** (gateway-restart fix).
    ///
    /// When an agent is built atop an existing journal, its
    /// durable context is in replay mode: every previously recorded
    /// `turn-{n}` (+ `turn-{n}-think`) is in the replay vector. However the
    /// gateway serves **new live messages** — it does NOT re-feed history.
    /// Without this call, the next live turn would:
    /// 1. use `turn_counter = 0` → step name `turn-0`, which would still hit
    ///    the still-open replay branch and crash with
    ///    [`DurableError::NondeterministicReplay`](familyclaw_durable::DurableError::NondeterministicReplay) (or silence the turn,
    ///    since `is_replaying()` gates LLM thinking and the reply), and
    /// 2. collide in memory's `turn_key` (`{name}:turn-0`) with the replay's
    ///    duplicate → the new message's memory would be lost (`MemoryStore` dedup).
    ///
    /// This builder does TWO interconnected things that must be done
    /// together:
    /// - **advances the durable cursor to the end of replay**
    ///   ([`DurableContext::fast_forward_replay`](familyclaw_durable::DurableContext::fast_forward_replay))
    ///   → the next step goes into the fresh-run branch at the correct sequence slot, and
    ///   `is_replaying()` is `false` → the agent thinks and replies again, and
    /// - **restores `turn_counter`** to the next free turn slot
    ///   ([`DurableContext::replayed_turn_count`](familyclaw_durable::DurableContext::replayed_turn_count))
    ///   → the new turn is `turn-{N}` (unique name + unique `turn_key`).
    ///
    /// Set **only on the persistent, live path** (the runtime's
    /// `build_family`, when `FAMILYCLAW_DATA_DIR` is set). The in-memory path
    /// (replay empty → no-op) and in-order re-feeding (continuity daemon /
    /// replay tests that feed the same history in order) do NOT
    /// call this — they want the replay to match step for step.
    ///
    /// Returns `self` for chaining ([`Agent::new`] signature unchanged).
    #[must_use]
    pub fn resume_live(mut self) -> Self {
        self.turn_counter = self.durable.replayed_turn_count();
        self.durable.fast_forward_replay();
        self
    }

    /// Set the **session isolation origin** (F4). After this, memories from
    /// turns handled by the agent are tagged with
    /// [`MessageOrigin::session_tag`](crate::session::MessageOrigin::session_tag),
    /// and [`Agent::think`]'s recall is filtered with the same tag — i.e. only
    /// **this session's** memories appear as context. One agent + one
    /// memory store suffice: isolation is done by tag, not by separate
    /// instances. The `None` state (default) preserves the shared scope
    /// (backward-compatible). Returns `self` for chaining.
    ///
    /// **F2 boundary:** once [`ResonanceMessage`] carries origin per message
    /// (F2 contract), this is replaced by a per-turn derivation
    /// ([`MessageOrigin::from_inbound_envelope`](crate::session::MessageOrigin::from_inbound_envelope)).
    #[must_use]
    pub fn with_session(mut self, origin: crate::session::MessageOrigin) -> Self {
        self.session = Some(origin);
        self
    }

    /// The agent's session origin (F4), if set.
    #[must_use]
    pub const fn session(&self) -> Option<&crate::session::MessageOrigin> {
        self.session.as_ref()
    }

    /// Wires the **full failover chain** onto the agent (replaces the
    /// 1-length chain built by [`Agent::new`]). Use this when you want
    /// primary + fallbacks: build the chain with [`build_llm_chain`](crate::build_llm_chain)
    /// ([`ModelConfig`](familyclaw_core::ModelConfig) → [`LlmFailover`]) and give
    /// it here. [`Agent::think`] then tries the chain's clients
    /// in order, until one succeeds (root-cause fix: the primary dying
    /// no longer kills the turn).
    ///
    /// Returns `self` for chaining; the [`Agent::new`] signature is not
    /// changed (backward-compatible).
    #[must_use]
    pub fn with_failover(mut self, failover: LlmFailover) -> Self {
        self.llm = Some(failover);
        self
    }

    /// Install the **emotion -> action governor** (Phase 1 emotion governor).
    /// `profile` is typically a [`GoverningProfile`] derived from Layer B's
    /// V130 calibration, but you can give any
    /// [`EmotionActionGoverning`] implementation (e.g. a mock test).
    ///
    /// When the governor is installed, [`Agent::handle_turn_with_origin`]:
    /// - **filters** `EmotionPulse` messages out of LLM thinking
    ///   (they are "blood", not speech)
    /// - **decides** the [`ActionDecision`] from the situational snapshot and
    ///   **suppresses the reply** if the decision is `Hesitate` or `Reflect` (safety net)
    /// - **suppresses the reply** entirely in `Hesitate` state
    ///
    /// When no governor is installed (default, backward-compatible),
    /// the agent behaves as before: thinks on all messages and
    /// always replies when it has an LLM.
    ///
    /// Returns `self` for chaining; the [`Agent::new`] signature is not
    /// changed.
    #[must_use]
    pub fn with_governor_profile(
        mut self,
        profile: Box<dyn EmotionActionGoverning + Send + Sync>,
    ) -> Self {
        self.governor = Some(profile);
        self
    }

    /// Install a governor using a wrapped [`GoverningProfile`]
    /// (a simpler API for the common case — no need to
    /// hand-wrap `Box<dyn>`).
    #[must_use]
    pub fn with_governing_profile(mut self, profile: GoverningProfile) -> Self {
        self.governor = Some(Box::new(profile));
        self
    }

    /// Install the **default governor** (conservative `default_governing_profile`).
    /// Same as `with_governing_profile(default_governing_profile())`,
    /// shorter.
    #[must_use]
    pub fn with_default_governor(mut self) -> Self {
        self.governor = Some(Box::new(default_governing_profile()));
        self
    }

    /// Install the **emotion engine calibration** (Layer B profile data).
    /// `calibration` is typically
    /// [`TableCalibration`](familyclaw_emotion::TableCalibration), loaded
    /// from the agent's `calibration.json`
    /// ([`TableCalibration::from_path`](familyclaw_emotion::TableCalibration::from_path)).
    ///
    /// When a non-neutral calibration is installed, the emotion state
    /// recovers toward the dimension's `baseline` rest state (not always
    /// zero) and contact stimuli are scaled by the dimension's
    /// `sensitivity` coefficient. Without this (default,
    /// [`NeutralCalibration`]) the agent behaves as before — fully
    /// backward-compatible.
    ///
    /// Returns `self` for chaining; the [`Agent::new`] signature is not
    /// changed.
    #[must_use]
    pub fn with_calibration(
        mut self,
        calibration: Box<dyn EmotionCalibration + Send + Sync>,
    ) -> Self {
        self.calibration = calibration;
        self
    }

    /// The agent's emotion engine calibration's identifying name (for logging).
    #[must_use]
    pub fn calibration_label(&self) -> &str {
        self.calibration.label()
    }

    /// Install the **action runtime** for the tool loop (Phase 1 keystone).
    ///
    /// Once the runtime is installed, [`Agent::think`] switches from the
    /// **single-shot** path to the tool loop: it builds
    /// [`ToolDefinition`]s to offer the LLM from the runtime's published
    /// [`McpToolDescriptor`] descriptors, gives them to the model, and routes
    /// the model's chosen tool calls back to the runtime
    /// ([`ActionRuntime::submit_task`]) until the model stops requesting
    /// tools or the [`ToolLoopConfig`] limit is reached.
    ///
    /// **Additive + backward-compatible:** without this call
    /// (`actions = None`) `think()` behaves exactly as before — one
    /// LLM call, no tools. Existing paths (gateway, tests)
    /// remain unchanged until the runtime is installed explicitly.
    ///
    /// `runtime` is given shared ([`Arc`] + [`Mutex`]), because
    /// [`ActionRuntime::submit_task`] is `&mut self` but `think()` borrows
    /// `&self`. Returns `self` for chaining ([`Agent::new`] signature
    /// unchanged).
    #[must_use]
    pub fn with_actions(mut self, runtime: Arc<Mutex<ActionRuntime>>) -> Self {
        self.actions = Some(runtime);
        self
    }

    /// Adjust the **tool loop safety limit** (round count per turn). Only in
    /// effect when [`with_actions`](Agent::with_actions) is installed. Returns
    /// `self` for chaining.
    #[must_use]
    pub const fn with_tool_loop(mut self, config: ToolLoopConfig) -> Self {
        self.tool_loop = config;
        self
    }

    /// The agent's tool loop configuration (read).
    #[must_use]
    pub const fn tool_loop(&self) -> ToolLoopConfig {
        self.tool_loop
    }

    /// Install the **storage surface for resumable turns** (suspend/resume
    /// bridge, roadmap §6).
    ///
    /// Give a crash-resistant [`crate::resumable::JournalResumableStore`], and
    /// the state of a turn suspended by the tool loop **survives a process
    /// crash**, and [`Agent::resume_approved`] can finish it
    /// after a restart, once approval is granted. The default surface
    /// ([`Agent::new`]) is in-memory and does not survive a crash.
    ///
    /// Returns `self` for chaining ([`Agent::new`] signature unchanged).
    #[must_use]
    pub fn with_resumable_store(mut self, store: Arc<dyn ResumableTurnStore>) -> Self {
        self.resumable = store;
        self
    }

    /// The agent's storage surface for resumable turns (shared handle e.g.
    /// for external inspection or an operator surface).
    #[must_use]
    pub fn resumable_store(&self) -> Arc<dyn ResumableTurnStore> {
        Arc::clone(&self.resumable)
    }

    /// Install the **turn audit collector** (TURN-AUDIT, roadmap §6 D6).
    ///
    /// Once the collector is installed, the tool loop's lifecycle becomes
    /// observable: for every turn the following are recorded —
    /// - **start** ([`AuditKind::TurnStarted`]),
    /// - **every tool call** ([`AuditKind::ToolDispatched`]) with the skill
    ///   name and a **redacted** result (never a raw payload),
    /// - **suspend** ([`AuditKind::TurnSuspended`]) with `approval_id` +
    ///   redacted summary,
    /// - **resume** ([`AuditKind::TurnResumed`]) when a suspended turn is resumed,
    /// - **`stop_reason`** ([`AuditKind::TurnAnswered`] /
    ///   [`AuditKind::TurnMaxIterations`] / [`AuditKind::TurnSuspended`]).
    ///
    /// The collector is given shared ([`Arc`]), so the operator surface (e.g.
    /// the gateway's route) can read the same trace the agent writes.
    /// Reading happens via
    /// [`turn_audit`](Agent::turn_audit) / [`turn_audit_for`](Agent::turn_audit_for).
    ///
    /// **Additive + backward-compatible:** without this call
    /// (`turn_audit = None`) the tool loop behaves exactly as before — no
    /// recording. Returns `self` for chaining ([`Agent::new`] signature
    /// unchanged).
    #[must_use]
    pub fn with_turn_audit(mut self, audit: Arc<AuditCollector>) -> Self {
        self.turn_audit = Some(audit);
        self
    }

    /// The agent's **turn audit collector**, if installed (shared handle).
    ///
    /// The operator can read the entire recorded trace
    /// ([`AuditCollector::list`]) or filter to a single turn
    /// ([`turn_audit_for`](Agent::turn_audit_for)). `None` if no audit has
    /// been wired ([`with_turn_audit`](Agent::with_turn_audit)).
    #[must_use]
    pub fn turn_audit(&self) -> Option<Arc<AuditCollector>> {
        self.turn_audit.clone()
    }

    /// Retrieves **a single turn's full audit trace** by its correlation
    /// identifier (TURN-AUDIT, roadmap §6 D6).
    ///
    /// `turn_id` is the [`ActionId`] that [`handle_turn_with_origin`] /
    /// [`resume_approved`] generates at the start of the turn, and with
    /// which all events of that same turn (start → tool calls →
    /// suspend/resume → `stop_reason`) are marked. Returns events in
    /// insertion order, or an empty list if no audit is wired or there are
    /// no events for the identifier.
    ///
    /// The output never contains raw secrets — every event's
    /// `detail` was already redacted at the moment of recording.
    ///
    /// [`handle_turn_with_origin`]: Agent::handle_turn_with_origin
    /// [`resume_approved`]: Agent::resume_approved
    #[must_use]
    pub fn turn_audit_for(&self, turn_id: ActionId) -> Vec<ExecAuditEvent> {
        self.turn_audit
            .as_ref()
            .map(|a| a.events_for(turn_id))
            .unwrap_or_default()
    }

    /// Records a single turn-audit event, if a collector is installed (TURN-AUDIT).
    ///
    /// **Secrecy invariant (defense in depth):** `detail` is run through
    /// [`familyclaw_actions::redact_free_text`] before storage, so a
    /// secret embedded in free text (a lone `sk-…` token, `Bearer …`,
    /// `key=value`) is redacted even if the caller accidentally supplies an
    /// unredacted string. `turn_id` correlates all events of the same turn.
    /// `at` (D1) is injected — the clock is not read here.
    ///
    /// No-op when no audit is wired (`turn_audit = None`).
    fn record_turn_audit(
        &self,
        turn_id: ActionId,
        kind: AuditKind,
        at: Timestamp,
        detail: impl Into<String>,
    ) {
        // Phase 2: every FRESH (non-replay) tool call bumps the tool metric.
        // Bound to this one audit-recording point, which is already called at
        // EVERY dispatch site (both tool loops) → covers everything without
        // repeating the emit call per site. Replay guard here: replay
        // must not double-count (the audit record, by contrast, is also made
        // during replay, so the emit is guarded separately).
        if matches!(kind, AuditKind::ToolDispatched) && !self.durable.is_replaying() {
            self.emit_metric(MetricEvent::ToolDispatched);
        }
        let Some(audit) = self.turn_audit.as_ref() else {
            return;
        };
        // Defense in depth: redact any secrets possibly embedded in free text
        // before storage. Discard the redaction report (we only need
        // the cleaned string).
        let (safe_detail, _) = familyclaw_actions::redact_free_text(&detail.into());
        audit.record(ExecAuditEvent::new(kind, turn_id, at, safe_detail));
    }

    /// Pushes a [`MetricEvent`] into the observability sink if installed.
    ///
    /// **Call ONLY on a fresh turn** (`!self.durable.is_replaying()`) —
    /// replay must not double-count metrics. Uses [`Sender::try_send`]:
    /// if the channel is full (consumer lagging) or closed, the event
    /// is **dropped** — observability must not block the turn nor grow
    /// the queue unboundedly. No-op when there is no sink.
    fn emit_metric(&self, event: MetricEvent) {
        if let Some(sink) = self.metrics_sink.as_ref() {
            let _ = sink.try_send(event);
        }
    }

    /// Whether the agent has an action runtime installed (tool loop active)?
    #[must_use]
    pub const fn has_actions(&self) -> bool {
        self.actions.is_some()
    }

    /// The agent's display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// The agent's bus identifier.
    #[must_use]
    pub const fn being_id(&self) -> BeingId {
        self.being_id
    }

    /// The agent's configuration (read).
    #[must_use]
    pub const fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// The agent's loaded soul (read).
    #[must_use]
    pub const fn soul(&self) -> &Soul {
        &self.soul
    }

    /// Current emotion state (read).
    #[must_use]
    pub const fn emotion(&self) -> &EmotionState {
        &self.emotion
    }

    /// Shared memory handle (e.g. for external lookup in tests).
    #[must_use]
    pub fn memory(&self) -> ErasedMemoryStore {
        Arc::clone(&self.memory)
    }

    /// Number of turns processed.
    #[must_use]
    pub const fn turns_taken(&self) -> u64 {
        self.turn_counter
    }

    /// The agent's LLM failover chain (optional).
    #[must_use]
    pub const fn llm(&self) -> Option<&LlmFailover> {
        self.llm.as_ref()
    }

    /// The agent's sandbox (optional).
    #[must_use]
    pub fn sandbox(&self) -> Option<Arc<dyn CodeSandbox>> {
        self.sandbox.clone()
    }

    /// Executes code in the sandbox (a tool for the LLM).
    ///
    /// Returns a tool response containing stdout/stderr and fuel consumption.
    ///
    /// # Errors
    /// - [`FamilyClawError::Sandbox`] if the sandbox is not configured or execution fails.
    pub fn execute_code(&self, wasm_bytes: Vec<u8>) -> Result<SandboxOutput> {
        let sandbox = self
            .sandbox
            .as_ref()
            .ok_or_else(|| FamilyClawError::sandbox("sandbox not configured"))?;

        let request = SandboxRequest::new(wasm_bytes);
        sandbox
            .execute(&request)
            .map_err(|e| FamilyClawError::sandbox(e.to_string()))
    }

    /// The agent's thinking: fetches relevant memories from the Eternal
    /// Thread (RAG), builds the system prompt (soul + memories), and calls
    /// the LLM.
    ///
    /// Natively **async** — no `block_on`/`block_in_place` patterns, which
    /// would panic on a `current_thread` runtime or could deadlock.
    ///
    /// ## Two paths (Phase 1 keystone)
    /// - **`actions = None` (default, UNCHANGED):** a single LLM call
    ///   ([`LlmFailover::complete`]) without tools. This is the original
    ///   single-shot behavior, which is not changed — all old paths and
    ///   tests remain intact.
    /// - **`actions = Some(rt)`:** an internal `run_tool_loop` runs the loop: the LLM
    ///   is given the tools published by the runtime, and its chosen tool
    ///   calls are routed back to the runtime until the model replies
    ///   without tool calls (a stop) or the [`ToolLoopConfig`] limit is reached.
    ///
    /// Returns a [`ThinkOutcome`] (1C, roadmap amendment 3): suspend is a
    /// STATE, not a string. The earlier `Option<Result<String>>` return has
    /// been replaced — see [`ThinkOutcome`]'s documentation for the
    /// rationale behind the migration.
    ///
    /// - No LLM client → [`ThinkOutcome::NoReply`] (harmless no-op).
    /// - Single-shot path → the model's text as [`ThinkOutcome::Reply`].
    /// - Tool loop `Answer` → [`ThinkOutcome::Reply`].
    /// - Tool loop `AwaitingApproval` → [`ThinkOutcome::Suspended`]
    ///   (id + redacted summary).
    /// - Tool loop `MaxIterations` / no text → [`ThinkOutcome::NoReply`].
    ///
    /// (`Answer`/`AwaitingApproval`/`MaxIterations` are variants of the tool
    /// loop's internal `ToolLoopOutcome` type — private mechanisms that
    /// `think` converts into the public [`ThinkOutcome`] above.)
    ///
    /// ## User-boundary protection (tool loop)
    /// Only [`ThinkOutcome::Reply`] is meant for the end user.
    /// [`ThinkOutcome::Suspended`] **never** travels through the reply pipe —
    /// it is recorded to the turn's durable state for resume
    /// ([`handle_turn_with_origin`](Self::handle_turn_with_origin)) — and its
    /// internal identifiers (including the raw `approval_id`) are not routed
    /// to the user. This fixes the 1B leak, where intermediate tokens leaked
    /// verbatim.
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] if the LLM call fails.
    pub async fn think(&self, current_message: &BusMessage) -> Result<ThinkOutcome> {
        self.think_with_origin(current_message, None).await
    }

    /// Like [`think`](Self::think), but knows the **turn's origin** (resume
    /// bridge, roadmap §6): when the tool loop suspends pending approval,
    /// the resumable turn ([`ResumableTurn`]) is saved with `conversation_origin`,
    /// so resume knows how to route the reply to the correct conversation.
    ///
    /// [`think`](Self::think) is a wrapper around this with `origin = None`
    /// (a static reply target).
    ///
    /// ## Suspend persists the resumable turn (EXACTLY ONCE)
    /// In the `AwaitingApproval` branch, this method builds a secret-free
    /// [`ResumableTurn`] (message stack + hashed arguments + identifiers) and
    /// saves it to the resumable turn store
    /// ([`resumable_store`](Self::resumable_store)) **before** returning
    /// [`ThinkOutcome::Suspended`]. The caller runs `think_with_origin` only
    /// on a FRESH turn (not during replay), so the put happens exactly once.
    ///
    /// **Determinism (D1):** the clock is read **once** (`time::now()`) at the
    /// start of this method and injected into the entire tool loop and into
    /// the resumable turn's `created_at` field — the loop logic does not read
    /// the clock itself.
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] if the LLM call fails.
    // A turn is one unified sequence (context → tool loop → result →
    // turn audit). TURN-AUDIT records (start/answered/suspend/max-iter)
    // pushed the line count slightly over the cap; splitting it would break
    // up the outcome mapping without a clarity benefit.
    #[allow(clippy::too_many_lines)]
    pub async fn think_with_origin(
        &self,
        current_message: &BusMessage,
        origin: Option<&familyclaw_bus::MessageOrigin>,
    ) -> Result<ThinkOutcome> {
        // No LLM client → no reply for this turn (harmless no-op).
        let Some(llm) = self.llm.as_ref() else {
            return Ok(ThinkOutcome::NoReply);
        };
        let (system_prompt, query) = self.build_think_context(current_message, origin).await;

        // Short-term memory: fetch this conversation's prior turns (RAM +
        // store hydration on cold start), so the model sees continuity.
        let conv_key = self.conversation_key(origin);
        let history = self.conversation_history(&conv_key, origin).await;

        match self.actions.as_ref() {
            // Single-shot path (backward-compatible): one LLM call,
            // no tools. Same behavior as before the tool loop → text as Reply.
            None => {
                let messages = build_message_stack(system_prompt, &history, query);
                let text = self
                    .llm_complete_with_progress(llm, &messages, origin)
                    .await?;
                Ok(ThinkOutcome::Reply(text))
            }
            // Tool loop path: give the model tools and loop until it
            // stops requesting them (or the limit is reached). Only `Answer` → `Reply`
            // crosses the user boundary; control states convert to Suspended/NoReply.
            //
            // D1: the clock is read ONCE here, injected into the tool loop.
            Some(actions) => {
                let now = time::now();
                // TURN-AUDIT (roadmap §6 D6): one correlation identifier for
                // this turn, with which all its events (start → tool calls →
                // stop_reason) are marked. No-op when no audit is wired.
                let turn_id = ActionId::new();
                self.record_turn_audit(turn_id, AuditKind::TurnStarted, now, "turn started");
                let messages = build_message_stack(system_prompt, &history, query);
                match self
                    .run_tool_loop(llm, actions, messages, now, turn_id)
                    .await?
                {
                    ToolLoopOutcome::Answer(text) => {
                        // stop_reason = answered. Do NOT record the reply text (may
                        // contain user data) — only the length, for observability.
                        self.record_turn_audit(
                            turn_id,
                            AuditKind::TurnAnswered,
                            now,
                            format!("answered ({} chars)", text.chars().count()),
                        );
                        Ok(ThinkOutcome::Reply(text))
                    }
                    ToolLoopOutcome::AwaitingApproval {
                        tool,
                        approval_id,
                        redacted_summary,
                        messages,
                        tool_call_id,
                        arguments,
                    } => {
                        // Suspend is a STATE: a tool is waiting for human approval.
                        // NOT to the user — `approval_id` is the operator's
                        // (ActionRuntime) information. We return it as a first-class
                        // Suspended state, which the caller records to durable state
                        // for resume (not into the reply pipe).
                        //
                        // Resume bridge (roadmap §6): save the resumable turn
                        // persistently, so `resume_approved` can continue the loop
                        // from where it left off — even across a process crash, if
                        // the surface is crash-resistant. `arguments` is given ONLY
                        // to be hashed (ResumableTurn::new computes the SHA-256,
                        // does not store the raw value). `now` (D1) is the
                        // resumable turn's `created_at`; the TTL is derived from the
                        // pending approval's expiry, if known.
                        let expires_at = self
                            .pending_expiry_for(actions, approval_id)
                            .await
                            .unwrap_or_else(|| {
                                now + chrono::Duration::minutes(RESUMABLE_DEFAULT_TTL_MINUTES)
                            });
                        // Secrecy invariant: redact the message stack's
                        // tool call arguments before saving to disk —
                        // raw payload/keys never end up on the durable surface.
                        let safe_messages = redact_messages_for_resume(&messages);
                        let resumable = ResumableTurn::new(
                            approval_id,
                            self.being_id.to_string(),
                            origin.cloned(),
                            safe_messages,
                            tool_call_id,
                            tool.clone(),
                            &arguments,
                            redacted_summary.clone(),
                            now,
                            expires_at,
                        )
                        .with_policy_snapshot(format!("tool '{tool}' requires human approval"))
                        .with_durable_position(self.turn_counter, 0);
                        if let Err(e) = self.resumable.put(resumable) {
                            // A persistence failure must not crash the turn,
                            // but resume then won't succeed → log as a warning.
                            warn!(
                                agent = self.config.name,
                                %approval_id,
                                error = %e,
                                "resumable turn persist failed — resume will not be possible for this approval"
                            );
                        }
                        debug!(
                            agent = self.config.name,
                            tool = tool.as_str(),
                            %approval_id,
                            "tool loop: awaiting human approval — suspending turn (resumable persisted, not routed to user)"
                        );
                        // stop_reason = suspended. `approval_id` + redacted
                        // summary — NOT a raw payload (the summary is already
                        // operator-safe, and redact_free_text still protects it).
                        self.record_turn_audit(
                            turn_id,
                            AuditKind::TurnSuspended,
                            now,
                            format!(
                                "suspended awaiting approval {approval_id}: {redacted_summary}"
                            ),
                        );
                        Ok(ThinkOutcome::Suspended {
                            approval_id,
                            redacted_summary,
                        })
                    }
                    ToolLoopOutcome::MaxIterations { iterations } => {
                        debug!(
                            agent = self.config.name,
                            iterations,
                            "tool loop: reached max iterations without a final answer — attempting recovery reply"
                        );
                        self.record_turn_audit(
                            turn_id,
                            AuditKind::TurnMaxIterations,
                            now,
                            format!("reached max iterations ({iterations}) without answer"),
                        );
                        Ok(ThinkOutcome::Reply(recovery_fallback_reply()))
                    }
                }
            }
        }
    }

    /// `FAMILYCLAW_STREAMING=1` → the LLM response is streamed and progress is updated every ~2s.
    fn llm_streaming_enabled() -> bool {
        std::env::var("FAMILYCLAW_STREAMING")
            .ok()
            .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    }

    /// One LLM call; with streaming, sends [`OutboundKind::Progress`] updates.
    async fn llm_complete_with_progress(
        &self,
        llm: &LlmFailover,
        messages: &[LlmMessage],
        origin: Option<&familyclaw_bus::MessageOrigin>,
    ) -> Result<String> {
        if !Self::llm_streaming_enabled() {
            return llm
                .complete(messages)
                .await
                .map_err(|e| FamilyClawError::llm(e.to_string()));
        }
        let target = origin
            .map(familyclaw_bus::MessageOrigin::reply_target)
            .or(self.reply_target.as_deref());
        let mut stream = llm
            .complete_stream(messages)
            .await
            .map_err(|e| FamilyClawError::llm(e.to_string()))?;
        let mut full = String::new();
        let emit_progress = should_emit_public_progress(origin);
        let mut last_progress = Instant::now();
        let frames = ["▱▱▱", "▰▱▱", "▰▰▱", "▰▰▰"];
        let mut frame_idx = 0usize;
        let mut progress_gate = ProgressGate::new();
        while let Some(chunk) = stream.next().await {
            let delta = chunk.map_err(|e| FamilyClawError::llm(e.to_string()))?;
            full.push_str(&delta);
            if emit_progress
                && last_progress.elapsed() >= PROGRESS_MIN_INTERVAL
                && progress_gate.allow()
            {
                if let (Some(sink), Some(target)) = (&self.reply_sink, target) {
                    let body = format!("↳ {} Drafting response…", frames[frame_idx]);
                    if let Ok(msg) = OutboundMessage::progress(target, body) {
                        let _ = sink.send(msg);
                        progress_gate.record();
                    }
                    frame_idx = (frame_idx + 1) % frames.len();
                }
                last_progress = Instant::now();
            }
        }
        Ok(full)
    }

    /// Builds a conversation key from the message's origin for short-term memory.
    ///
    /// Key = `"{channel_id}:{conversation}"` — the same format as the F4
    /// session key, but derived from the per-message
    /// [`familyclaw_bus::MessageOrigin`]. If there is no origin (e.g. an
    /// internal/test message without a channel), the reply target is used
    /// as a fallback, and ultimately a shared `"default"` key. This way, an
    /// agent that doesn't yet have origin information uses one shared
    /// history instead of losing continuity entirely.
    fn conversation_key(&self, origin: Option<&familyclaw_bus::MessageOrigin>) -> String {
        if let Some(o) = origin {
            return format!("{}:{}", o.channel_id, o.conversation);
        }
        self.reply_target
            .clone()
            .unwrap_or_else(|| "default".to_string())
    }

    /// Returns the conversation's short-term memory messages (oldest→newest)
    /// for building the LLM stack. Uses the in-process RAM window first; if
    /// empty (cold start / new process), hydrates the most recent
    /// `chat:*`-tagged memories with the session scope.
    async fn conversation_history(
        &self,
        conv_key: &str,
        origin: Option<&MessageOrigin>,
    ) -> Vec<LlmMessage> {
        let ram = self.history_for(conv_key);
        if !ram.is_empty() {
            return ram;
        }
        self.hydrate_history_from_store(origin).await
    }

    /// Fetches the most recent session-scoped chat messages from the memory store.
    async fn hydrate_history_from_store(&self, origin: Option<&MessageOrigin>) -> Vec<LlmMessage> {
        let Some(session_tag) = self.session_tag_for_recall(origin) else {
            return Vec::new();
        };
        let all = match self.memory.all().await {
            Ok(m) => m,
            Err(e) => {
                warn!("chat history hydration failed (non-fatal): {e}");
                return Vec::new();
            }
        };
        let mut chat: Vec<(Timestamp, LlmMessage)> = all
            .into_iter()
            .filter(|m| {
                m.source == CHAT_HISTORY_SOURCE
                    && m.tags.iter().any(|t| t == &session_tag)
                    && m.status == familyclaw_memory::MemoryStatus::Active
            })
            .filter_map(|m| {
                let role = if m.tags.iter().any(|t| t == CHAT_USER_TAG) {
                    Some(LlmMessage::user(truncate_for_history(&m.content)))
                } else if m.tags.iter().any(|t| t == CHAT_ASSISTANT_TAG) {
                    Some(LlmMessage::assistant(truncate_for_history(&m.content)))
                } else {
                    None
                };
                role.map(|msg| (m.created_at, msg))
            })
            .collect();
        chat.sort_by_key(|a| a.0);
        if chat.len() > HISTORY_HYDRATE_LIMIT {
            chat.drain(0..chat.len() - HISTORY_HYDRATE_LIMIT);
        }
        chat.into_iter().map(|(_, msg)| msg).collect()
    }

    /// Derives the session tag per message from origin or the static `with_session`.
    fn session_tag_for_recall(&self, origin: Option<&MessageOrigin>) -> Option<String> {
        origin.map(session_tag_from_origin).or_else(|| {
            self.session
                .as_ref()
                .map(crate::session::MessageOrigin::session_tag)
        })
    }

    /// Saves a single chat-role message to the memory store with the session
    /// scope (for cold-start hydration). Non-duplicate: `turn_key` is
    /// missing → every turn is its own entry.
    async fn persist_chat_turn(
        &self,
        origin: &MessageOrigin,
        role_tag: &str,
        text: &str,
    ) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let memory = Memory::builder(truncate_for_history(text))
            .source(CHAT_HISTORY_SOURCE)
            .decay_policy(DecayPolicy::Normal)
            .tags([session_tag_from_origin(origin), role_tag.to_string()])
            .build();
        self.memory
            .add(memory)
            .await
            .map_err(|e| FamilyClawError::memory(format!("chat history persist failed: {e}")))?;
        Ok(())
    }

    /// Returns the conversation's short-term memory messages (oldest→newest)
    /// for building the LLM stack. Empty if there's no history yet for the conversation.
    fn history_for(&self, conv_key: &str) -> Vec<LlmMessage> {
        self.history
            .get(conv_key)
            .map(|dq| dq.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Appends a successful turn (user message + agent reply) to the
    /// conversation's short-term memory as a sliding window. Truncates each
    /// message to [`HISTORY_MAX_CHARS_PER_MSG`] and drops the oldest once
    /// the count exceeds [`HISTORY_MAX_MESSAGES`].
    ///
    /// **Call ONLY on a fresh turn** (`!self.durable.is_replaying()`) —
    /// otherwise replay would double-record. Empty messages are not saved.
    fn append_history(&mut self, conv_key: &str, user_text: &str, assistant_text: &str) {
        let user_text = user_text.trim();
        let assistant_text = assistant_text.trim();
        if user_text.is_empty() || assistant_text.is_empty() {
            return;
        }
        let dq = self.history.entry(conv_key.to_string()).or_default();
        dq.push_back(LlmMessage::user(truncate_for_history(user_text)));
        dq.push_back(LlmMessage::assistant(truncate_for_history(assistant_text)));
        while dq.len() > HISTORY_MAX_MESSAGES {
            dq.pop_front();
        }
    }

    /// Builds [`think`](Agent::think)'s shared context: RAG recall +
    /// system prompt (soul essence + memories) and the message text (`query`).
    ///
    /// Shared between both paths (single-shot + tool loop), so that memory
    /// retrieval and prompt building are identical regardless of whether
    /// tools are installed. F4 session isolation: if a session is set,
    /// recall requires the session tag (only that session's memories are visible).
    #[allow(clippy::format_push_string)]
    async fn build_think_context(
        &self,
        current_message: &BusMessage,
        origin: Option<&MessageOrigin>,
    ) -> (String, String) {
        let query = bus_message_text(current_message);

        // ORIENT: fetch relevant memories FIRST (RAG — before the LLM call).
        let semantic_weight = std::env::var("FAMILYCLAW_SEMANTIC_WEIGHT")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .map_or(0.6, |w| w.clamp(0.0, 1.0));
        let mut recall_ctx = RetrievalContext::new(query.clone())
            .with_limit(5)
            .with_semantic_weight(semantic_weight);
        if let Some(tag) = self.session_tag_for_recall(origin) {
            recall_ctx = recall_ctx.with_required_tags([tag]);
        }
        let memories = self.recall(&recall_ctx).await.unwrap_or_else(|e| {
            warn!("recall failed in think (non-fatal): {e}");
            Vec::new()
        });
        let memories = crate::identity::filter_memories_for_operator(memories, origin, |hit| {
            hit.memory.content.as_str()
        });

        // System prompt: soul essence + memories as context.
        let mut system_prompt = self.soul.essence.clone();
        system_prompt.push_str(&crate::identity::identity_guard_prompt(origin));
        if !memories.is_empty() {
            system_prompt.push_str("\n\n[RELEVANT MEMORIES FROM ETERNAL THREAD]:\n");
            for (i, mem) in memories.iter().enumerate() {
                system_prompt.push_str(&format!(
                    "  {}. (relevance: {:.2}) {}\n",
                    i + 1,
                    mem.relevance,
                    mem.memory.content
                ));
            }
            system_prompt.push_str("[END MEMORIES]\n");
        }
        if self.actions.is_some() {
            system_prompt.push_str(
                "\n[TOOL CONTRACT]\n\
                 Disk and web changes require tool calls in THIS turn. \
                 Never claim DONE/evidence/file paths without a tool_result. \
                 If unsure, say what tool you will call next.\n",
            );
        }

        (system_prompt, query)
    }

    /// Runs the **tool loop** journaled durably (D1, roadmap §6 green-gate e):
    /// the same engine as [`run_tool_loop`](Self::run_tool_loop), but every
    /// tool dispatch ([`ActionRuntime::submit_task`]) is wrapped in its own
    /// durable step `turn-{turn}-dispatch-{k}`, so **partial progress
    /// within a turn survives a crash**.
    ///
    /// ## Why this exists (red-team finding)
    /// Without this, the loop only journals the entire `think` as a single
    /// `-think` step **after the loop**. If the process crashes BETWEEN two
    /// tool dispatches, nothing has been recorded yet → replay runs the
    /// entire `think` from scratch → (a) the first tool's side effect runs
    /// AGAIN, and (b) [`ActionRuntime::submit_task`] produces a NEW random
    /// [`ApprovalId`] ([`uuid::Uuid::new_v4`], not clock-derived) →
    /// determinism breaks. Per-dispatch journaling closes the gap: during
    /// replay an already-recorded dispatch is returned from the log
    /// (`SubmitOutcome` value-identical, including the random `ApprovalId` +
    /// clock-derived TTL) **without** running the skill executor again.
    ///
    /// `durable` is a `&mut` context, so this is a **free function that takes
    /// fields separately** (not `&self`): `handle_turn_with_origin` can borrow
    /// `&mut self.durable` and the other `self` fields immutably as separate
    /// (disjoint field borrows) — as a `&self` method the borrows would overlap.
    ///
    /// `dispatch_base` is the number of dispatches already recorded for this
    /// turn (usually 0 for a fresh turn); it continues `-dispatch-{k}`
    /// numbering from the correct point during replay.
    ///
    /// `being_id` is **this being's** identifier (usually the string form of
    /// [`Agent::being_id`]). It is passed to
    /// [`ActionRuntime::submit_task_as`], so the **per-being rate limit** for
    /// dangerous (approval-requiring) tool calls applies to the correct
    /// being and does not fall back to the runtime's generic default — this
    /// way one being cannot consume another being's quota through the same
    /// shared runtime.
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] if the LLM call fails unrecoverably.
    /// - [`FamilyClawError::Bus`] if a durable step fails (wrapped).
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn drive_tool_loop_durable(
        llm: &LlmFailover,
        actions: &Arc<Mutex<ActionRuntime>>,
        durable: &mut DurableContext<ErasedJournal>,
        turn: u64,
        dispatch_base: u32,
        agent_name: &str,
        being_id: &str,
        turn_audit: Option<&Arc<AuditCollector>>,
        metrics_sink: Option<&MetricEventSink>,
        progress_sink: Option<ReplySink>,
        progress_target: Option<String>,
        mut messages: Vec<LlmMessage>,
        mut last_text: String,
        budget: u32,
        now: Timestamp,
        turn_id: ActionId,
        origin: Option<&MessageOrigin>,
        user_query: &str,
    ) -> Result<(ToolLoopOutcome, u32)> {
        // Phase 2: emit the tool-call metric if a sink is installed. Called
        // at every dispatch site guarded by `!replaying` (replay must not
        // double-count). Generic, carries no user/Layer B data.
        let emit_tool = |replaying: bool| {
            if !replaying {
                if let Some(sink) = metrics_sink {
                    // try_send: drop on overflow, do not block/grow the queue unbounded.
                    let _ = sink.try_send(MetricEvent::ToolDispatched);
                }
            }
        };
        // `-dispatch-{k}` and `-llm-{k}` running numbers: continue from the
        // correct point after replay. Shared across ALL rounds (not reset
        // per round), so every step gets a unique, deterministic name.
        let mut dispatch_index = dispatch_base;
        let emit_progress = should_emit_public_progress(origin);
        let mut last_progress_label: Option<String> = None;
        let mut progress_gate = ProgressGate::new();
        let mut tool_use_counts: HashMap<String, u32> = HashMap::new();
        let mut fs_read_count = 0u32;
        let mut dispatch_count = 0u32;
        for iteration in 0..budget {
            // D1: wrap **the LLM call too** in a durable step. Without this,
            // replay would call the LLM again (non-deterministic + a network
            // call), and replay of an already-fully-completed turn would
            // diverge. On a fresh run the call is made and its result
            // (content + tool_calls) is journaled; during replay the saved
            // result is returned without making the network call. A
            // serializable projection is journaled (`CompletionResult` is not
            // `Serialize`). `iteration` is the loop's sequence number →
            // deterministic step name.
            let llm_step = format!("turn-{turn}-llm-{iteration}");
            let replaying_llm = durable.is_replaying();
            let (content, tool_calls_opt): (Option<String>, Option<Vec<ToolCall>>) =
                if replaying_llm {
                    durable
                        .step(&llm_step, || {
                            Err("unreachable: replay returns journaled LLM result".to_string())
                        })
                        .map_err(|e| {
                            FamilyClawError::bus(format!("durable llm replay failed: {e}"))
                        })?
                } else {
                    let tools = {
                        let rt = actions.lock().await;
                        build_tool_definitions(&rt.tool_definitions())
                    };
                    let result = llm
                        .complete_with_tools(&messages, &tools)
                        .await
                        .map_err(|e| FamilyClawError::llm(e.to_string()))?;
                    let mut content = result.content;
                    let mut tool_calls_opt = result.tool_calls;
                    let no_tools = tool_calls_opt.as_ref().is_none_or(Vec::is_empty);
                    if no_tools
                        && iteration == 0
                        && dispatch_index == dispatch_base
                        && !tools.is_empty()
                        && crate::grounding::looks_like_action_request(user_query)
                    {
                        let retry = llm
                            .complete_with_tools_choice(&messages, &tools, Some("required"))
                            .await
                            .map_err(|e| FamilyClawError::llm(e.to_string()))?;
                        content = retry.content;
                        tool_calls_opt = retry.tool_calls;
                    }
                    let projection = (content.clone(), tool_calls_opt.clone());
                    durable
                        .step(&llm_step, {
                            let projection = projection.clone();
                            move || Ok(projection)
                        })
                        .map_err(|e| {
                            FamilyClawError::bus(format!("durable llm step failed: {e}"))
                        })?;
                    (content, tool_calls_opt)
                };

            let text = content.clone().unwrap_or_default();
            if !text.is_empty() {
                last_text = text;
            }

            let Some(tool_calls) = tool_calls_opt.filter(|c| !c.is_empty()) else {
                let answer = content.filter(|c| !c.is_empty()).unwrap_or(last_text);
                let guarded = crate::grounding::apply_grounding_guard(&answer, dispatch_count);
                return Ok((ToolLoopOutcome::Answer(guarded), dispatch_count));
            };

            messages.push(
                LlmMessage::assistant(content.unwrap_or_default())
                    .with_tool_calls(tool_calls.clone()),
            );

            for call in tool_calls {
                let tool_key = call.name.clone();
                let per_tool = tool_use_counts.entry(tool_key.clone()).or_insert(0);
                *per_tool += 1;
                let is_fs_read = tool_key.contains("fs_read");
                if is_fs_read {
                    fs_read_count += 1;
                }
                if *per_tool > TOOL_BUDGET_PER_NAME
                    || (is_fs_read && fs_read_count > TOOL_BUDGET_FS_READ)
                {
                    emit_tool(durable.is_replaying());
                    record_turn_audit_into(
                        turn_audit,
                        turn_id,
                        AuditKind::ToolDispatched,
                        now,
                        format!("tool '{}' skipped: per-turn budget exceeded", call.name),
                    );
                    messages.push(LlmMessage::tool_result(
                        call.id,
                        "SYSTEM: Tool budget exceeded for this turn. Reply to the operator now with your best answer — do not call more tools.",
                    ));
                    continue;
                }

                let Some(skill_id) = actions.lock().await.map_name_to_skill(&call.name) else {
                    emit_tool(durable.is_replaying());
                    record_turn_audit_into(
                        turn_audit,
                        turn_id,
                        AuditKind::ToolDispatched,
                        now,
                        format!("tool '{}' dispatched: unknown tool", call.name),
                    );
                    messages.push(LlmMessage::tool_result(
                        call.id,
                        format!("error: unknown tool '{}'", call.name),
                    ));
                    continue;
                };

                // D1: wrap the dispatch side effect + result in ITS OWN durable
                // step. In a fresh run the executor runs and the `SubmitOutcome`
                // (random `ApprovalId` + clock-derived TTL) is serialized to the
                // journal; on replay the stored `SubmitOutcome` is returned without
                // running the executor again → side effect exactly once, ApprovalId/TTL
                // value-identical. `now` is injected (not `time::now()` inside the closure).
                let dispatch_step = format!("turn-{turn}-dispatch-{dispatch_index}");
                dispatch_index += 1;
                let replaying = durable.is_replaying();
                if emit_progress && !replaying {
                    if let (Some(sink), Some(target)) = (&progress_sink, &progress_target) {
                        let label = tool_progress_label(&call.name);
                        let should_send = last_progress_label.as_deref() != Some(label.as_str())
                            && progress_gate.allow();
                        if should_send {
                            let step = dispatch_index;
                            let body = format!("↳ Step {step} · {label}");
                            if let Ok(msg) = OutboundMessage::progress(target, body) {
                                let _ = sink.send(msg);
                                progress_gate.record();
                            }
                            last_progress_label = Some(label);
                        }
                    }
                }
                // Both the replay and fresh-run branches journal/replay the SAME
                // type ([`DispatchRecord`]), so the serde format matches (the step
                // name and type are deterministic under replay). Any submit
                // error is carried in the `DispatchRecord::error` field, NOT
                // as the step's own error — this way replay returns the same record.
                let record: DispatchRecord = if replaying {
                    // Replay branch: the step is already in the journal → return the
                    // stored record without running `submit_task` again. The closure
                    // is NOT run on replay, so its `Ok` value is irrelevant (but its
                    // TYPE must be `DispatchRecord` so serde matches).
                    durable
                        .step(&dispatch_step, || {
                            Ok(DispatchRecord {
                                task_id: ActionTaskId::nil(),
                                status: familyclaw_actions::task::TaskStatus::Failed,
                                pending_approval: None,
                                error: Some(
                                    "unreachable: replay returns journaled value".to_string(),
                                ),
                            })
                        })
                        .map_err(|e| {
                            FamilyClawError::bus(format!("durable dispatch replay failed: {e}"))
                        })?
                } else {
                    // Fresh-run branch: run `submit_task` (the side effect) NOW, in the
                    // proper async context, and record its result in the step. We do not
                    // run it inside the `step` closure (the closure is synchronous) — we
                    // run it before wrapping and journal the finished result.
                    //
                    // 🔑 KEYSTONE (exactly-once across a SIGKILL boundary): the dispatch
                    // runs **idempotently** through the runtime's outbox, keyed by the
                    // same deterministic `dispatch_step` name. This closes the window
                    // BETWEEN the side effect's (`submit_task`) execution and the
                    // `durable.step` journaling BELOW it: if the process is killed in
                    // between, the side effect is already committed in the outbox, so a
                    // restart/replay returns the same outcome without running the side
                    // effect again — regardless of whether the journal row managed to be
                    // created. (`submit_task_as` alone does not protect this window.)
                    let outcome = {
                        let mut rt = actions.lock().await;
                        // Submit the task under THIS being's (`being_id`) name, so that
                        // the per-being rate limit for approval-requiring tools applies
                        // correctly and does not collapse onto the runtime's generic
                        // default being.
                        rt.submit_task_idempotent(
                            &dispatch_step,
                            being_id,
                            skill_id,
                            call.arguments.clone(),
                            now,
                        )
                        .await
                    };
                    let record = DispatchRecord::from_outcome(&outcome);
                    durable
                        .step(&dispatch_step, {
                            let record = record.clone();
                            move || Ok(record)
                        })
                        .map_err(|e| {
                            FamilyClawError::bus(format!("durable dispatch step failed: {e}"))
                        })?
                };

                if let Some(err) = record.error.clone() {
                    // Execution error (journaled): feed the error back to the model
                    // (it may correct the call), CONTINUE within the budget. Same
                    // behavior as the non-durable path; replay returns the same error.
                    {
                        let e = err;
                        warn!(
                            agent = agent_name,
                            tool = call.name.as_str(),
                            error = %e,
                            "tool loop (durable): submit_task failed — feeding error result, continuing"
                        );
                        emit_tool(replaying);
                        record_turn_audit_into(
                            turn_audit,
                            turn_id,
                            AuditKind::ToolDispatched,
                            now,
                            format!("tool '{}' dispatched: failed: {e}", call.name),
                        );
                        messages.push(LlmMessage::tool_result(
                            call.id,
                            format_tool_failure_for_model(&call.name, &e),
                        ));
                        continue;
                    }
                }

                if let Some(approval_id) = record.pending_approval {
                    // Tool requiring approval → INTERNAL control state. The redacted
                    // summary is fetched from the pending record (no secrets).
                    let redacted_summary = {
                        let rt = actions.lock().await;
                        rt.pending_summary_for(approval_id).unwrap_or_else(|| {
                            format!("tool '{}' awaiting human approval", call.name)
                        })
                    };
                    emit_tool(replaying);
                    record_turn_audit_into(
                        turn_audit,
                        turn_id,
                        AuditKind::ToolDispatched,
                        now,
                        format!(
                            "tool '{}' dispatched: awaiting approval ({redacted_summary})",
                            call.name
                        ),
                    );
                    return Ok((
                        ToolLoopOutcome::AwaitingApproval {
                            tool: call.name.clone(),
                            approval_id,
                            redacted_summary,
                            messages: messages.clone(),
                            tool_call_id: call.id.clone(),
                            arguments: call.arguments.clone(),
                        },
                        dispatch_count,
                    ));
                }

                if record.error.is_none() {
                    dispatch_count = dispatch_count.saturating_add(1);
                }

                // Safe / auto-run: feed the (redacted) result back to the model.
                // The proof is fetched from the runtime by task_id (fresh + replay:
                // the same task_id was journaled, so the proof is found on both
                // paths in a fresh run; on replay the proof may be missing →
                // tool_result_text falls back to a status description, which is
                // acceptable because replay never leaks out to the user).
                let result_text = {
                    let rt = actions.lock().await;
                    tool_result_text_for(&rt, record.task_id, record.status)
                };
                emit_tool(replaying);
                record_turn_audit_into(
                    turn_audit,
                    turn_id,
                    AuditKind::ToolDispatched,
                    now,
                    format!("tool '{}' dispatched: {result_text}", call.name),
                );
                messages.push(LlmMessage::tool_result(call.id, result_text));
            }
        }

        if last_text.is_empty() {
            if let Some(recovered) = recover_user_visible_reply(llm, &messages).await {
                let guarded = crate::grounding::apply_grounding_guard(&recovered, dispatch_count);
                return Ok((ToolLoopOutcome::Answer(guarded), dispatch_count));
            }
            Ok((
                ToolLoopOutcome::MaxIterations { iterations: budget },
                dispatch_count,
            ))
        } else {
            let guarded = crate::grounding::apply_grounding_guard(&last_text, dispatch_count);
            Ok((ToolLoopOutcome::Answer(guarded), dispatch_count))
        }
    }

    /// Disk-backed operator status (no LLM theater).
    fn execute_operator_status_bootstrap(&mut self, step_name: &str) -> Result<String> {
        let bootstrap_step = format!("{step_name}-operator-status");
        if self.durable.is_replaying() {
            return self
                .durable
                .step(&bootstrap_step, || {
                    Err("unreachable: replay returns journaled status reply".to_string())
                })
                .map_err(|e| FamilyClawError::bus(format!("status replay failed: {e}")));
        }
        let home = crate::identity::operator_home_root().ok_or_else(|| {
            FamilyClawError::config(
                "operator status requires FAMILYCLAW_FS_READ_ALLOW or FAMILYCLAW_FILE_WRITE_ALLOW"
                    .to_string(),
            )
        })?;
        let reply = crate::identity::operator_status_report(&home);
        self.durable
            .step(&bootstrap_step, {
                let reply = reply.clone();
                move || Ok(reply)
            })
            .map_err(|e| FamilyClawError::bus(format!("status step failed: {e}")))?;
        Ok(reply)
    }

    /// Handles operator APPROVE/DENY chat commands via `ActionRuntime`.
    async fn handle_operator_approval_command(
        &self,
        message: &BusMessage,
        origin: Option<&MessageOrigin>,
    ) -> Result<Option<String>> {
        if !crate::identity::is_operator_origin(origin) || self.durable.is_replaying() {
            return Ok(None);
        }
        let Some(cmd) = crate::identity::parse_operator_approval_command(message) else {
            return Ok(None);
        };
        let Some(actions) = self.actions.as_ref() else {
            return Ok(Some("ActionRuntime ei ole kytketty.".to_string()));
        };
        let now = time::now();
        match cmd {
            crate::identity::OperatorApprovalCommand::Approve(id_str) => {
                let id: ApprovalId = id_str.parse().map_err(|_| {
                    FamilyClawError::config(format!("invalid approval id: {id_str}"))
                })?;
                {
                    let mut rt = actions.lock().await;
                    rt.approve(id, now)
                        .await
                        .map_err(|e| FamilyClawError::bus(e.to_string()))?;
                }
                self.handle_resume_signal(&id.to_string(), now).await?;
                Ok(Some(format!(
                    "APPROVE OK: {id} — resume signaali lähetetty."
                )))
            }
            crate::identity::OperatorApprovalCommand::Deny(id_str) => {
                let id: ApprovalId = id_str.parse().map_err(|_| {
                    FamilyClawError::config(format!("invalid approval id: {id_str}"))
                })?;
                let mut rt = actions.lock().await;
                rt.deny_pending(id, now)
                    .await
                    .map_err(|e| FamilyClawError::bus(e.to_string()))?;
                Ok(Some(format!("DENY OK: {id} — tehtävä peruutettu.")))
            }
        }
    }

    async fn record_verified_tool_memory(&self, dispatch_count: u32, summary: &str) -> Result<()> {
        let snippet: String = summary.chars().take(240).collect();
        let content = format!("verified tool turn (dispatch={dispatch_count}): {snippet}");
        let memory = Memory::builder(content)
            .source("tool_verified")
            .tags(["verified:tool".to_string()])
            .build();
        self.memory.add(memory).await?;
        Ok(())
    }

    /// Deterministic TOP 20 #1 bootstrap for operator "Tee se!" / "JATKA".
    ///
    /// Runs real `file_write_allowlisted` tasks (memory.md + research/log.md),
    /// journals the final reply for replay, and never hallucinates completion.
    async fn execute_operator_top20_bootstrap(&mut self, step_name: &str) -> Result<String> {
        let bootstrap_step = format!("{step_name}-operator-bootstrap");
        if self.durable.is_replaying() {
            return self
                .durable
                .step(&bootstrap_step, || {
                    Err("unreachable: replay returns journaled bootstrap reply".to_string())
                })
                .map_err(|e| FamilyClawError::bus(format!("bootstrap replay failed: {e}")));
        }

        let actions = self.actions.as_ref().ok_or_else(|| {
            FamilyClawError::config("operator bootstrap requires ActionRuntime".to_string())
        })?;
        let home = crate::identity::operator_home_root().ok_or_else(|| {
            FamilyClawError::config(
                "operator bootstrap requires FAMILYCLAW_FILE_WRITE_ALLOW or FAMILYCLAW_FS_READ_ALLOW"
                    .to_string(),
            )
        })?;
        let now = time::now();
        let now_iso = familyclaw_core::time::to_rfc3339(now);
        let writes = crate::identity::operator_top20_bootstrap_plan(&home, &now_iso);
        let being_id = self.being_id.to_string();
        let mut written = Vec::new();
        let mut blocked: Option<String> = None;

        for (idx, (rel_display, payload)) in writes.into_iter().enumerate() {
            let dispatch_step = format!("{step_name}-bootstrap-{idx}");
            let skill_id = {
                let rt = actions.lock().await;
                rt.map_name_to_skill("file_write_allowlisted")
                    .ok_or_else(|| {
                        FamilyClawError::config(
                            "file_write_allowlisted not registered in ActionRuntime".to_string(),
                        )
                    })?
            };
            let outcome = {
                let mut rt = actions.lock().await;
                rt.submit_task_idempotent(&dispatch_step, &being_id, skill_id, payload, now)
                    .await?
            };
            if outcome.awaiting_approval() {
                blocked = Some(format!(
                    "file_write odottaa hyväksyntää (task {})",
                    outcome.task_id
                ));
                break;
            }
            if outcome.status != familyclaw_actions::task::TaskStatus::Done {
                blocked = Some(format!(
                    "file_write status {:?} for {rel_display}",
                    outcome.status
                ));
                break;
            }
            written.push(rel_display);
        }

        let reply = if let Some(reason) = blocked {
            crate::identity::operator_top20_bootstrap_blocked_reply(&reason)
        } else {
            crate::identity::operator_top20_bootstrap_done_reply(&home, &written)
        };

        self.durable
            .step(&bootstrap_step, {
                let reply = reply.clone();
                move || Ok(reply)
            })
            .map_err(|e| FamilyClawError::bus(format!("bootstrap step failed: {e}")))?;
        Ok(reply)
    }

    /// **Fresh actions-turn thinking, durable-journaled** (D1 production
    /// path). Runs [`drive_tool_loop_durable`](Self::drive_tool_loop_durable)
    /// over `&mut self.durable`, records the outcome (`-think` text or
    /// `-suspend` summary), and persists any resumable turn —
    /// exactly like the actions branch of [`think_with_origin`](Self::think_with_origin),
    /// but every tool dispatch is in its own durable step, so
    /// partial progress within the turn survives a crash.
    ///
    /// Handles BOTH a fresh run AND a replay: the loop checks
    /// `durable.is_replaying()` per dispatch, so in a replayed turn already
    /// journaled dispatches are returned from the journal without running the
    /// executor again, and only past the end of the journal does it continue fresh.
    /// Finally the `-think`/`-suspend` step closes the turn (same naming as the
    /// single-shot path).
    ///
    /// Returns `(thought, suspend)`: at most one is `Some`
    /// (mutually exclusive). `now` is read **once** and injected through the whole loop
    /// (D1) — the clock is not read inside the loop logic.
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] if the LLM call fails unrecoverably.
    /// - [`FamilyClawError::Bus`] if a durable step fails.
    // The fresh/replay branches + Answer/Suspend/MaxIter outcome mapping +
    // resumable persistence form one logical unit; splitting it up would
    // fragment the turn-closing logic without a clarity benefit.
    #[allow(clippy::type_complexity, clippy::too_many_lines)]
    async fn think_actions_durable(
        &mut self,
        message: &BusMessage,
        origin: Option<&familyclaw_bus::MessageOrigin>,
        turn: u64,
    ) -> Result<(Option<String>, Option<(ApprovalId, String)>)> {
        // Fail-closed: if the LLM or the action runtime is not installed,
        // this path is a no-op (the single-shot path handles the reply). NO
        // `expect`/panic on the production path — return a harmless "no answer"
        // (same semantics as the former `is_none` guard, but without a separate
        // `expect` unwrap later on).
        let Some(actions) = self.actions.as_ref() else {
            return Ok((None, None));
        };
        // Clone the shared handles before the `&mut self.durable` borrow, so that
        // disjoint-borrow works (Arc handles + owned values, not `&self`).
        let actions = Arc::clone(actions);
        let max_iterations = self.tool_loop.max_iterations;
        let agent_name = self.config.name.clone();
        let being_id = self.being_id;
        let turn_audit = self.turn_audit.clone();
        // Clone the metrics sink before the `&mut self.durable` borrow (disjoint-borrow).
        let metrics_sink = self.metrics_sink.clone();

        let (system_prompt, query) = self.build_think_context(message, origin).await;
        // Short-term memory: include this conversation's earlier turns (RAM + store).
        let conv_key = self.conversation_key(origin);
        let history = self.conversation_history(&conv_key, origin).await;
        let now = time::now();
        let turn_id = ActionId::new();
        // TURN-AUDIT records do not belong to a replayed turn (the trail was
        // already recorded in the original run); record only in a fresh loop.
        let audit = if self.durable.is_replaying() {
            None
        } else {
            turn_audit.as_ref()
        };
        record_turn_audit_into(audit, turn_id, AuditKind::TurnStarted, now, "turn started");

        let messages = build_message_stack(system_prompt, &history, query.clone());
        // The LLM handle is an `LlmFailover` (not `Clone`); it is read from `self`
        // separately at the same time as `&mut self.durable` — disjoint field
        // borrow works because `llm` and `durable` are different fields.
        //
        // Fail-closed: if the LLM handle is (no longer) present, return harmlessly
        // without an answer — NO `expect`/panic on the production path. (The
        // earlier `actions` guard does not guarantee the LLM's presence, so this
        // is checked separately right before borrowing the handle.)
        let Some(llm) = self.llm.as_ref() else {
            return Ok((None, None));
        };
        let progress_sink = if should_emit_public_progress(origin) {
            self.reply_sink.clone()
        } else {
            None
        };
        let progress_target = if should_emit_public_progress(origin) {
            self.reply_target_for_origin(origin)
        } else {
            None
        };
        let outcome = Self::drive_tool_loop_durable(
            llm,
            &actions,
            &mut self.durable,
            turn,
            0,
            &agent_name,
            &being_id.to_string(),
            turn_audit.as_ref(),
            metrics_sink.as_ref(),
            progress_sink,
            progress_target,
            messages,
            String::new(),
            max_iterations,
            now,
            turn_id,
            origin,
            &query,
        )
        .await?;

        let think_step = format!("turn-{turn}-think");
        match outcome.0 {
            ToolLoopOutcome::Answer(text) => {
                if outcome.1 > 0 && !self.durable.is_replaying() {
                    let _ = self.record_verified_tool_memory(outcome.1, &text).await;
                }
                record_turn_audit_into(
                    audit,
                    turn_id,
                    AuditKind::TurnAnswered,
                    now,
                    format!("answered ({} chars)", text.chars().count()),
                );
                // Close the turn with the `-think` step (returned from the journal on replay).
                let thought = self
                    .durable
                    .step(&think_step, {
                        let text = text.clone();
                        move || Ok(text)
                    })
                    .map_err(|e| FamilyClawError::bus(format!("durable think step failed: {e}")))?;
                Ok((Some(thought).filter(|s| !s.is_empty()), None))
            }
            ToolLoopOutcome::AwaitingApproval {
                tool,
                approval_id,
                redacted_summary,
                messages,
                tool_call_id,
                arguments,
            } => {
                // Close `-think` as empty, so the replay cursor stays aligned
                // (suspend produces a separate `-suspend` entry below).
                let _ = self
                    .durable
                    .step(&think_step, || Ok(String::new()))
                    .map_err(|e| FamilyClawError::bus(format!("durable think step failed: {e}")))?;

                // Persist the resumable turn (resume bridge) — only on a fresh
                // run (on replay it was already persisted in the original run).
                if !self.durable.is_replaying() {
                    let expires_at = self
                        .pending_expiry_for(&actions, approval_id)
                        .await
                        .unwrap_or_else(|| {
                            now + chrono::Duration::minutes(RESUMABLE_DEFAULT_TTL_MINUTES)
                        });
                    let safe_messages = redact_messages_for_resume(&messages);
                    let resumable = ResumableTurn::new(
                        approval_id,
                        being_id.to_string(),
                        origin.cloned(),
                        safe_messages,
                        tool_call_id,
                        tool.clone(),
                        &arguments,
                        redacted_summary.clone(),
                        now,
                        expires_at,
                    )
                    .with_policy_snapshot(format!("tool '{tool}' requires human approval"))
                    .with_durable_position(turn, 0);
                    if let Err(e) = self.resumable.put(resumable) {
                        warn!(
                            agent = agent_name,
                            %approval_id,
                            error = %e,
                            "resumable turn persist failed — resume will not be possible for this approval"
                        );
                    }
                }
                record_turn_audit_into(
                    audit,
                    turn_id,
                    AuditKind::TurnSuspended,
                    now,
                    format!("suspended awaiting approval {approval_id}: {redacted_summary}"),
                );
                // `-suspend` entry (same format as the single-shot path):
                // "<approval_id>|<redacted_summary>" — no raw payload.
                let suspend_step = format!("turn-{turn}-suspend");
                let payload = format!("{approval_id}|{redacted_summary}");
                if let Err(e) = self.durable.step(&suspend_step, {
                    let payload = payload.clone();
                    move || Ok(payload)
                }) {
                    warn!("durable suspend step failed (non-fatal): {e}");
                }
                Ok((None, Some((approval_id, redacted_summary))))
            }
            ToolLoopOutcome::MaxIterations { iterations } => {
                record_turn_audit_into(
                    audit,
                    turn_id,
                    AuditKind::TurnMaxIterations,
                    now,
                    format!("reached max iterations ({iterations}) without answer"),
                );
                let text = recovery_fallback_reply();
                let thought = self
                    .durable
                    .step(&think_step, {
                        let text = text.clone();
                        move || Ok(text)
                    })
                    .map_err(|e| FamilyClawError::bus(format!("durable think step failed: {e}")))?;
                Ok((Some(thought).filter(|s| !s.is_empty()), None))
            }
        }
    }

    /// Runs the **tool loop** (Phase 1 keystone): a SpatialClaw-style loop
    /// in which the model calls a tool, the result is fed back, and it
    /// cycles until it stops.
    ///
    /// ## Steps per round
    /// 1. Build tool definitions from the runtime's published MCP descriptions
    ///    ([`ActionRuntime::tool_definitions`] → [`ToolDefinition`]) — only
    ///    valid ones ([`ToolDefinition::validate`]) are offered to the model.
    /// 2. Call [`LlmFailover::complete_with_tools`].
    /// 3. **No tool calls** → return the model's text (stop).
    /// 4. For each tool call:
    ///    - **unknown tool** ([`ActionRuntime::map_name_to_skill`] = `None`)
    ///      → push an error `tool_result` and CONTINUE (consumes the round,
    ///      does not abort and does not get stuck in an infinite retry),
    ///    - **requires approval** ([`SubmitOutcome::pending_approval`] = `Some`)
    ///      → return [`ToolLoopOutcome::AwaitingApproval`] (an internal control
    ///      state with a typed `approval_id` + redacted summary, NOT to the
    ///      user); [`think`](Self::think) translates it into
    ///      [`ThinkOutcome::Suspended`],
    ///    - **safe / auto-run** → push the result as a `tool_result` and continue.
    /// 5. **Budget exhausted** → return [`ToolLoopOutcome::Answer`] (the latest text)
    ///    or [`ToolLoopOutcome::MaxIterations`] (no answer).
    ///
    /// ## User boundary
    /// Only [`ToolLoopOutcome::Answer`] is intended for the end user.
    /// The control states ([`ToolLoopOutcome::AwaitingApproval`],
    /// [`ToolLoopOutcome::MaxIterations`]) are internal to the developer — their
    /// separation at the type level prevents a transient marker (e.g. a raw
    /// `approval_id`) from leaking through the reply pipeline to the user.
    ///
    /// Never panics: all error paths return as a [`Result`] or
    /// continue the loop within the budget.
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] if the LLM call fails unrecoverably.
    async fn run_tool_loop(
        &self,
        llm: &LlmFailover,
        actions: &Arc<Mutex<ActionRuntime>>,
        messages: Vec<LlmMessage>,
        now: Timestamp,
        turn_id: ActionId,
    ) -> Result<ToolLoopOutcome> {
        // Run the loop from a fresh message stack with the full round budget.
        // `now` is injected (D1): the clock is not read inside the loop logic,
        // so that task dispatch uses this same, journalable timestamp.
        // `turn_id` (TURN-AUDIT) correlates the loop's tool calls to this turn.
        self.drive_tool_loop(
            llm,
            actions,
            messages,
            String::new(),
            self.tool_loop.max_iterations,
            now,
            turn_id,
            // Fresh turn: no idempotent continuation key (former behavior).
            None,
        )
        .await
    }

    /// **Shared engine** for the tool loop: runs the loop from the given
    /// message stack until the model stops, a tool requires approval, or the
    /// round budget is exhausted.
    ///
    /// Shared between two entry points, so the logic is exactly one:
    /// - [`run_tool_loop`](Self::run_tool_loop) — fresh turn (system + user).
    /// - [`resume_approved`](Self::resume_approved) — resumed turn: restored
    ///   message stack + the already-fed result of the approved tool.
    ///
    /// `budget` is the remaining round count (resume continues with the same
    /// overall budget, it does not reset it). `last_text` is the latest model
    /// text (typically empty on resume). Behavior is otherwise identical
    /// to the original `run_tool_loop` — see its phase description.
    ///
    /// **Idempotent dispatch (`idempotent_key_prefix`):**
    /// - `None` → fresh turn: dispatch via [`ActionRuntime::submit_task_as`]
    ///   (former behavior, unchanged — this path is already durable-protected
    ///   via [`drive_tool_loop_durable`] in production).
    /// - `Some(prefix)` → **post-approval continuation** (resume): every
    ///   tool dispatch runs **idempotently**
    ///   ([`ActionRuntime::submit_task_idempotent`]) keyed by
    ///   `{prefix}-dispatch-{k}`, where `k` is the continuation's internal
    ///   dispatch number running across all rounds. The key is
    ///   **deterministic across a restart**: the same `prefix` (derived from
    ///   the approval id) + the same dispatch index → the same key, so the
    ///   outbox deduplicates a crash-then-replay situation and the side
    ///   effect does not fire twice. This closes the last double-fire window
    ///   that remained on the non-durable dispatch path AFTER an approval was
    ///   granted.
    ///
    /// **Determinism (D1):** `now` is injected — the clock is **not** read
    /// inside the loop logic; all task dispatches use this same timestamp.
    /// This lets the caller journal the timestamp inside the step (value
    /// identical on replay) so the loop is not non-deterministic.
    ///
    /// **Turn audit (roadmap §6 D6):** `turn_id` correlates every tool call
    /// of this loop ([`AuditKind::ToolDispatched`]) to one turn.
    /// Events are only recorded if a collector is installed
    /// ([`with_turn_audit`](Self::with_turn_audit)); otherwise a no-op. `detail`
    /// is redacted before storage (no raw payload).
    ///
    /// # Errors
    /// - [`FamilyClawError::Llm`] if the LLM call fails unrecoverably.
    // The tool loop engine is one tight loop; the arguments (llm, actions,
    // messages, last_text, budget, now, turn_id) are all state needed to
    // run the loop, and TURN-AUDIT added one more (`turn_id`).
    // Bundling them into a context struct would add indirection with no clarity benefit.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn drive_tool_loop(
        &self,
        llm: &LlmFailover,
        actions: &Arc<Mutex<ActionRuntime>>,
        mut messages: Vec<LlmMessage>,
        mut last_text: String,
        budget: u32,
        now: Timestamp,
        turn_id: ActionId,
        idempotent_key_prefix: Option<&str>,
    ) -> Result<ToolLoopOutcome> {
        // Post-approval continuation dispatch number: running across ALL
        // rounds (not reset per round), so every idempotent dispatch key
        // is unique and deterministic under replay.
        // Only used when `idempotent_key_prefix` is `Some`.
        let mut dispatch_index: u32 = 0;
        for _ in 0..budget {
            // 1. Build tools from the runtime's MCP descriptions (lock held
            //    only for the duration of the descriptions — released before the LLM call).
            let tools = {
                let rt = actions.lock().await;
                build_tool_definitions(&rt.tool_definitions())
            };

            // 2. LLM call with tools.
            let result = llm
                .complete_with_tools(&messages, &tools)
                .await
                .map_err(|e| FamilyClawError::llm(e.to_string()))?;

            if !result.text().is_empty() {
                last_text = result.text().to_string();
            }

            // 3. No tool calls → the model stopped, return the text as the answer.
            //    Empty but present content (`Some("")`, which some
            //    OpenAI-compatible providers produce) is filtered out,
            //    so we return the earlier non-empty text instead of going silent.
            let Some(tool_calls) = result.tool_calls.filter(|c| !c.is_empty()) else {
                let answer = result
                    .content
                    .filter(|c| !c.is_empty())
                    .unwrap_or(last_text);
                return Ok(ToolLoopOutcome::Answer(answer));
            };

            // Append the model's assistant turn (with its tool calls) to the history,
            // so subsequent tool_result messages bind to the right call ids.
            messages.push(
                LlmMessage::assistant(result.content.unwrap_or_default())
                    .with_tool_calls(tool_calls.clone()),
            );

            // 4. Dispatch every tool call and feed the result back.
            for call in tool_calls {
                let Some(skill_id) = actions.lock().await.map_name_to_skill(&call.name) else {
                    // Unknown tool: feed back an error result, CONTINUE (consumes
                    // the round, does not abort the loop and does not get stuck in
                    // an infinite retry — the budget also bounds the error path).
                    debug!(
                        agent = self.config.name,
                        tool = call.name.as_str(),
                        "tool loop: unknown tool — feeding error result, continuing"
                    );
                    // TURN-AUDIT: the tool call was dispatched but the name was unknown.
                    // Only the skill name + status — no arguments and no payload.
                    self.record_turn_audit(
                        turn_id,
                        AuditKind::ToolDispatched,
                        now,
                        format!("tool '{}' dispatched: unknown tool", call.name),
                    );
                    messages.push(LlmMessage::tool_result(
                        call.id,
                        format!("error: unknown tool '{}'", call.name),
                    ));
                    continue;
                };

                let outcome = {
                    let mut rt = actions.lock().await;
                    // D1: injected `now` (not `time::now()` inside the loop) —
                    // the same timestamp that can be journaled deterministically.
                    // Submit under THIS being's (`being_id`) name, so the per-being
                    // rate limit for approval-requiring tools applies correctly and
                    // does not collapse onto the runtime's generic default being.
                    if let Some(prefix) = idempotent_key_prefix {
                        // 🔑 Post-approval continuation: dispatch idempotently
                        // with the deterministic key `{prefix}-dispatch-{k}`.
                        // `k` (dispatch_index) is stable across a restart (same prefix
                        // + same index → same key), so the outbox deduplicates a
                        // crash-then-replay situation and the side effect does not
                        // fire twice. The index is incremented BEFORE dispatch, so
                        // replay hits the same key in the same order.
                        let key = format!("{prefix}-dispatch-{dispatch_index}");
                        dispatch_index += 1;
                        rt.submit_task_idempotent(
                            &key,
                            &self.being_id.to_string(),
                            skill_id,
                            call.arguments.clone(),
                            now,
                        )
                        .await
                    } else {
                        // Fresh (non-continuation) turn: the former non-idempotent path.
                        // This path is already durable-protected via `drive_tool_loop_durable`
                        // in production; here `submit_task_as` preserves the
                        // original behavior byte-for-byte.
                        rt.submit_task_as(
                            &self.being_id.to_string(),
                            skill_id,
                            call.arguments.clone(),
                            now,
                        )
                        .await
                    }
                };

                match outcome {
                    Ok(submit) if submit.pending_approval.is_some() => {
                        // Tool requiring approval → return the INTERNAL control
                        // state [`ToolLoopOutcome::AwaitingApproval`], NOT a string
                        // to be routed to the user. `approval_id` remains in
                        // [`ActionRuntime`]'s state for the operator's later
                        // `approve` call — it is never sent to the end user. The
                        // turn does not hang, and the approval-requiring action
                        // does not execute without permission.
                        // [`think`](Self::think) translates this into a first-class
                        // [`ThinkOutcome::Suspended`].
                        //
                        // `pending_approval` is `Some` in this branch (the branch
                        // condition guarantees it), so we read the typed id directly.
                        // A redacted, operator-safe summary is fetched from the
                        // pending record — derived only from the skill's name and
                        // identifiers, not secrets. If the summary is not found for
                        // some reason, we use a neutral placeholder
                        // (never a raw payload/arguments).
                        let Some(approval_id) = submit.pending_approval else {
                            // Unreachable (branch condition = is_some), but we do not
                            // panic on the production path — continue the loop.
                            continue;
                        };
                        let redacted_summary = {
                            let rt = actions.lock().await;
                            rt.pending_summary_for(approval_id).unwrap_or_else(|| {
                                format!("tool '{}' awaiting human approval", call.name)
                            })
                        };
                        // TURN-AUDIT: the tool was dispatched, but requires approval
                        // (redacted summary — no arguments and no payload).
                        self.record_turn_audit(
                            turn_id,
                            AuditKind::ToolDispatched,
                            now,
                            format!(
                                "tool '{}' dispatched: awaiting approval ({redacted_summary})",
                                call.name
                            ),
                        );
                        // Resume state (roadmap §6): the message stack is in
                        // EXACTLY the right state for resuming — the assistant turn
                        // (with its tool calls) has already been appended (above), but
                        // THIS call's `tool_result` does NOT exist yet. Resume injects
                        // the approved tool's result into the `tool_call_id` and
                        // continues from the stack. `arguments` is given only to be
                        // hashed ([`ResumableTurn::new`] computes SHA-256, does not
                        // store the raw value). We clone the stack because the loop's
                        // own `messages` only moves out via this return.
                        return Ok(ToolLoopOutcome::AwaitingApproval {
                            tool: call.name.clone(),
                            approval_id,
                            redacted_summary,
                            messages: messages.clone(),
                            tool_call_id: call.id.clone(),
                            arguments: call.arguments.clone(),
                        });
                    }
                    Ok(submit) => {
                        // Safe / auto-run: feed the (redacted) result back to
                        // the model. The proof contains redacted output
                        // without secrets.
                        let result_text = {
                            let rt = actions.lock().await;
                            tool_result_text(&rt, &submit)
                        };
                        // TURN-AUDIT: the tool was dispatched and executed. The result is
                        // already redacted (`tool_result_text` uses the redacted
                        // proof), and `record_turn_audit` redacts it once more
                        // (defense in depth).
                        self.record_turn_audit(
                            turn_id,
                            AuditKind::ToolDispatched,
                            now,
                            format!("tool '{}' dispatched: {result_text}", call.name),
                        );
                        messages.push(LlmMessage::tool_result(call.id, result_text));
                    }
                    Err(e) => {
                        // Execution error: feed the error back to the model (it may
                        // correct the call), CONTINUE within the budget.
                        warn!(
                            agent = self.config.name,
                            tool = call.name.as_str(),
                            error = %e,
                            "tool loop: submit_task failed — feeding error result, continuing"
                        );
                        // TURN-AUDIT: the tool was dispatched but execution failed.
                        // The error text is redacted (it may reflect arguments).
                        self.record_turn_audit(
                            turn_id,
                            AuditKind::ToolDispatched,
                            now,
                            format!("tool '{}' dispatched: failed: {e}", call.name),
                        );
                        messages.push(LlmMessage::tool_result(
                            call.id,
                            format_tool_failure_for_model(&call.name, &e),
                        ));
                    }
                }
            }
        }

        // 5. Budget exhausted before stopping. NO panic. If the model managed
        //    to produce text, it is the best available answer → `Answer`.
        //    Otherwise return the INTERNAL control state [`ToolLoopOutcome::MaxIterations`]
        //    — a robotic max-iter marker is NOT routed to the user.
        //    `iterations` reports the budget given for this run (resume continues
        //    with the remaining budget, so the number reflects the actual limit).
        if last_text.is_empty() {
            if let Some(recovered) = recover_user_visible_reply(llm, &messages).await {
                return Ok(ToolLoopOutcome::Answer(recovered));
            }
            Ok(ToolLoopOutcome::MaxIterations { iterations: budget })
        } else {
            Ok(ToolLoopOutcome::Answer(last_text))
        }
    }

    /// Fetches the pending approval's expiration time from [`ActionRuntime`]
    /// (lock held only for the fetch). `None` if the approval is (no longer) pending.
    async fn pending_expiry_for(
        &self,
        actions: &Arc<Mutex<ActionRuntime>>,
        approval_id: ApprovalId,
    ) -> Option<Timestamp> {
        let rt = actions.lock().await;
        rt.pending_expiry_for(approval_id)
    }

    /// **Resumes a suspended turn once approval has been granted** (suspend/resume
    /// bridge, roadmap §6 — resume side).
    ///
    /// When [`think`](Self::think)/[`think_with_origin`](Self::think_with_origin)'s
    /// tool loop suspended awaiting approval, the resumable turn's state
    /// ([`ResumableTurn`]) was persisted to the resumable turn store
    /// ([`resumable_store`](Self::resumable_store)).
    /// This method:
    ///
    /// 1. **loads** the resumable turn by `approval_id` (fail-closed: unknown
    ///    or expired → error, no panic, no side effects),
    /// 2. **consumes the approval** ([`ActionRuntime::approve`]) → the suspended
    ///    action is executed to completion **exactly once** (payload-bound,
    ///    single-use — see [`familyclaw_actions::approval::ApprovalLedger::consume`]),
    /// 3. **injects** the approved tool's (redacted) result back into the
    ///    restored message stack, bound to the `tool_call_id`,
    /// 4. **continues the tool loop** from where it left off
    ///    (the internal tool loop engine) — the model may now respond
    ///    conclusively or request more tools (possibly a new suspend),
    /// 5. **consumes the resumable turn** (removes it from the store) after a
    ///    successful `approve`, so it cannot be resumed a second time.
    ///
    /// Returns a [`ThinkOutcome`]:
    /// - [`Reply`](ThinkOutcome::Reply) when the model produced a final answer,
    /// - [`Suspended`](ThinkOutcome::Suspended) when the continuation required a
    ///   **new** approval (a new resumable turn has then already been persisted),
    /// - [`NoReply`](ThinkOutcome::NoReply) when the continuation hit the round
    ///   budget without text, or there is no LLM client.
    ///
    /// ## Determinism (D1)
    /// `now` is **injected** — the clock is not read inside this method. The
    /// same timestamp governs both the approval consumption's expiry check AND
    /// the continued tool loop's task dispatches, so the caller can journal it
    /// inside the step (value identical on replay).
    ///
    /// ## User boundary + secrets
    /// Only `Reply` is intended for the user. Raw secrets were never stored in
    /// the resumable turn (see [`ResumableTurn`]), and the injected tool result
    /// is derived from a **redacted** proof.
    ///
    /// ## Isolation between beings (defense in depth)
    /// A resumable turn belongs to the being that persisted it. Resume
    /// **refuses** to continue a turn whose [`ResumableTurn::being_id`]
    /// differs from this agent's own identifier — one being cannot continue
    /// another being's suspended turn nor consume its approval.
    /// The check is done **before** consuming the approval, so a mismatch leaves
    /// no trace (fail-closed: no consumption, no removal, no side effect).
    ///
    /// # Errors
    /// - [`FamilyClawError::InvalidInput`] if there is no resumable turn for
    ///   `approval_id` (unknown/consumed), it belongs to **another
    ///   being** (ownership mismatch), it is expired, the agent has no
    ///   action runtime installed ([`with_actions`](Self::with_actions)),
    ///   or consuming the approval ([`ActionRuntime::approve`]) fails (e.g.
    ///   payload mismatch) — all fail-closed, no panic.
    /// - [`FamilyClawError::Llm`] if the continued LLM call fails.
    // Resume is a coherent sequence (load → consume approval → inject result
    // → turn-audit resumed → continue tool loop → map outcome + stop_reason).
    // The TURN-AUDIT records pushed the line count over the ceiling; splitting
    // this up would fragment this logical unit.
    #[allow(clippy::too_many_lines)]
    pub async fn resume_approved(
        &self,
        approval_id: ApprovalId,
        now: Timestamp,
    ) -> Result<ThinkOutcome> {
        // 1. Load the resumable turn fail-closed. Unknown/consumed → error
        //    (no panic, no side effects).
        let turn = self
            .resumable
            .get(approval_id)
            .map_err(|e| FamilyClawError::invalid_input(format!("resumable load failed: {e}")))?
            .ok_or_else(|| {
                FamilyClawError::invalid_input(format!(
                    "no resumable turn for approval {approval_id} (unknown or already resumed)"
                ))
            })?;

        // 1b. OWNERSHIP CHECK (isolation invariant, defense in depth).
        //
        // A resumable turn belongs exactly to the being that persisted it at
        // suspension time ([`ResumableTurn::being_id`]). The store can be shared
        // across multiple beings, and the key is just `approval_id` —
        // so without this check, one being could resume **another being's**
        // suspended turn (and consume its approval + run its side effect
        // in its own context). That would break the isolation between beings.
        //
        // **Invariant:** `turn.being_id == self.being_id`. This is
        // defense in depth — the caller should route resume to the correct
        // being before arriving here, but this still verifies it at the
        // boundary where the approval is about to be consumed.
        //
        // **Fail-closed:** a mismatch → error BEFORE consuming the approval and
        // before running any of the tool loop. The approval is NOT consumed, the
        // resumable turn is NOT removed, the side effect is NOT run — the
        // foreign being leaves empty-handed and no trace is left. No panic.
        let self_being = self.being_id.to_string();
        if turn.being_id != self_being {
            return Err(FamilyClawError::invalid_input(format!(
                "resumable turn for approval {approval_id} belongs to another being \
                 (owner mismatch) — refusing to resume across beings"
            )));
        }

        // An expired resumable turn is refused fail-closed (same boundary as
        // the approval) — the permission is not consumed, no side effect is run.
        if turn.is_expired(now) {
            return Err(FamilyClawError::invalid_input(format!(
                "resumable turn for approval {approval_id} expired"
            )));
        }

        // Resume requires an action runtime (the same runtime that granted the permission).
        let Some(actions) = self.actions.as_ref() else {
            return Err(FamilyClawError::invalid_input(
                "resume_approved requires an ActionRuntime (call with_actions first)".to_string(),
            ));
        };

        // 2. Consume the approval → the suspended action is executed to
        //    completion EXACTLY ONCE (payload-bound, single-use). `now` injected (D1).
        let submit = {
            let mut rt = actions.lock().await;
            rt.approve(approval_id, now)
                .await
                .map_err(|e| FamilyClawError::invalid_input(format!("approve failed: {e}")))?
        };

        // 3. Inject the approved tool's (redacted) result into the restored
        //    message stack, bound to the original tool_call_id.
        let mut messages = turn.messages;
        let result_text = {
            let rt = actions.lock().await;
            tool_result_text(&rt, &submit)
        };
        messages.push(LlmMessage::tool_result(turn.tool_call_id, result_text));

        // Approval consumed successfully → consume the resumable turn
        // (single-use: cannot be resumed twice). Done BEFORE continuing the loop,
        // so that a possible new suspend persists ITS OWN resumable turn
        // without the old one being left hanging.
        if let Err(e) = self.resumable.remove(approval_id) {
            warn!(
                agent = self.config.name,
                %approval_id,
                error = %e,
                "resumable remove after approve failed (non-fatal) — turn already advanced"
            );
        }

        // TURN-AUDIT (roadmap §6 D6): this is a resumed turn → a new
        // correlation id + a `TurnResumed` event. Recorded only once the
        // approval has been consumed successfully (above), so the audit trail
        // reflects an actual resume and not a failed attempt. No-op when
        // audit is not wired up.
        let turn_id = ActionId::new();
        self.record_turn_audit(
            turn_id,
            AuditKind::TurnResumed,
            now,
            format!("turn resumed after approval {approval_id}"),
        );

        // 4. Continue the tool loop from the restored stack. No LLM → NoReply.
        let Some(llm) = self.llm.as_ref() else {
            return Ok(ThinkOutcome::NoReply);
        };
        // 🔑 IDEMPOTENT CONTINUATION (closes the last double-fire window):
        // tool dispatches made AFTER the approval is granted are routed through
        // `submit_task_idempotent` with a deterministic key prefix derived from
        // this continuation's approval id. The same
        // `approval_id` + the same continuation-internal dispatch index produce the
        // same key across a restart, so a crash-then-replay does not fire the
        // side effect again. (Previously this path called `submit_task_as`
        // directly, which made post-approval dispatches susceptible
        // to double-firing in the crash window.)
        let resume_dispatch_prefix = format!("resume-{approval_id}");
        // Continue with the FULL round budget: resume is a new "episode" in which
        // the model gets room to proceed again. The safety limit still bounds an
        // infinite loop.
        // `turn_id` correlates the continued loop's tool calls to this resume turn.
        let outcome = self
            .drive_tool_loop(
                llm,
                actions,
                messages,
                String::new(),
                self.tool_loop.max_iterations,
                now,
                turn_id,
                Some(&resume_dispatch_prefix),
            )
            .await?;

        match outcome {
            ToolLoopOutcome::Answer(text) => {
                // stop_reason = answered.
                self.record_turn_audit(
                    turn_id,
                    AuditKind::TurnAnswered,
                    now,
                    format!("answered ({} chars)", text.chars().count()),
                );
                Ok(ThinkOutcome::Reply(text))
            }
            ToolLoopOutcome::AwaitingApproval {
                tool,
                approval_id: next_id,
                redacted_summary,
                messages,
                tool_call_id,
                arguments,
            } => {
                // The continuation required a NEW approval → persist a new resumable
                // turn (same invariant as the original suspend). The original
                // origin is preserved, so the reply is routed to the same
                // conversation even after a chained approval.
                let expires_at = self
                    .pending_expiry_for(actions, next_id)
                    .await
                    .unwrap_or_else(|| {
                        now + chrono::Duration::minutes(RESUMABLE_DEFAULT_TTL_MINUTES)
                    });
                let safe_messages = redact_messages_for_resume(&messages);
                let next_turn = ResumableTurn::new(
                    next_id,
                    self.being_id.to_string(),
                    turn.conversation_origin,
                    safe_messages,
                    tool_call_id,
                    tool.clone(),
                    &arguments,
                    redacted_summary.clone(),
                    now,
                    expires_at,
                )
                .with_policy_snapshot(format!("tool '{tool}' requires human approval"))
                .with_durable_position(self.turn_counter, 0);
                if let Err(e) = self.resumable.put(next_turn) {
                    warn!(
                        agent = self.config.name,
                        approval_id = %next_id,
                        error = %e,
                        "chained resumable turn persist failed — further resume not possible"
                    );
                }
                // stop_reason = suspended (chained new approval).
                self.record_turn_audit(
                    turn_id,
                    AuditKind::TurnSuspended,
                    now,
                    format!("suspended awaiting approval {next_id}: {redacted_summary}"),
                );
                Ok(ThinkOutcome::Suspended {
                    approval_id: next_id,
                    redacted_summary,
                })
            }
            ToolLoopOutcome::MaxIterations { iterations } => {
                self.record_turn_audit(
                    turn_id,
                    AuditKind::TurnMaxIterations,
                    now,
                    format!("reached max iterations ({iterations}) without answer"),
                );
                Ok(ThinkOutcome::Reply(recovery_fallback_reply()))
            }
        }
    }

    /// **The agent's side of the suspend/resume bridge**: handles the operator's
    /// approval-granted control signal
    /// ([`BusMessage::ResumeApproval`])
    /// by continuing the suspended turn to completion and pushing the final
    /// answer OUT to the reply sink.
    ///
    /// This is a **different path from a normal turn**: it does NOT start a new
    /// LLM turn and does not go through [`handle_turn_with_origin`](Self::handle_turn_with_origin),
    /// but is routed directly to [`resume_approved`](Self::resume_approved),
    /// which consumes the approval and continues the suspended tool loop from
    /// where it left off (idempotently). `now` (D1) is injected as on the resume path.
    ///
    /// ## Deriving the reply target (same logic as the normal route)
    /// The reply target is derived by **the same** rule as
    /// [`handle_turn_with_origin`](Self::handle_turn_with_origin)'s reply branch:
    /// primarily from the suspended turn's persisted
    /// [`conversation_origin`](crate::resumable::ResumableTurn::conversation_origin)
    /// (`reply_target()`), and if absent, from the agent's static
    /// [`with_reply_target`](Self::with_reply_target). The origin is peeked from
    /// the resumable turn **before** `resume_approved` consumes it;
    /// a peek error does not block continuation (fallback `None` → static target).
    ///
    /// ## Outcome mapping
    /// - [`ThinkOutcome::Reply`] → build an [`OutboundMessage`] and route it via
    ///   [`route_reply`](Self::route_reply) (NO bus publish → echo-loop protection).
    /// - [`ThinkOutcome::Suspended`] → the turn requires **further approval**;
    ///   no-op + `info!` (the operator grants the next approval separately).
    /// - [`ThinkOutcome::NoReply`] → no-op.
    ///
    /// ## Fail-closed (a single resume does not crash the actor)
    /// - Invalid `approval_id` (parse error): `warn!` + `Ok(())`, no panic.
    /// - `resume_approved` error (unknown/consumed/expired/ownership
    ///   mismatch): `warn!` + `Ok(())` — same error boundary as the turn handler.
    /// - Reply-routing failure (closed sink): `warn!`, does not crash.
    ///
    /// # Errors
    /// Always returns `Ok(())` — all errors are handled fail-closed
    /// (logged), so a single resume signal cannot crash the actor.
    pub async fn handle_resume_signal(&self, approval_id: &str, now: Timestamp) -> Result<()> {
        // Parse the approval identifier fail-closed: an invalid string → log +
        // no-op (no panic, no side effect).
        let Ok(id) = approval_id.parse::<ApprovalId>() else {
            warn!(
                agent = self.config.name,
                approval_id, "resume signal: invalid approval id — ignoring"
            );
            return Ok(());
        };

        // Peek the resumable turn's origin for the reply target BEFORE
        // `resume_approved` consumes (removes) the turn. A peek error does not
        // block continuation: fallback `None` → the agent's static reply target
        // (same as the normal route's fallback). No `?` propagation: we do not
        // want a peek error to block consuming the approval.
        let reply_origin = match self.resumable.get(id) {
            Ok(turn) => turn.and_then(|t| t.conversation_origin),
            Err(e) => {
                debug!(
                    agent = self.config.name,
                    %id, error = %e,
                    "resume signal: resumable peek failed — falling back to static reply target"
                );
                None
            }
        };

        // Continue the turn to completion. Error (unknown/consumed/expired/
        // ownership mismatch) → log + Ok (a single resume does not crash the
        // actor, same boundary as the turn handler).
        let outcome = match self.resume_approved(id, now).await {
            Ok(outcome) => outcome,
            Err(e) => {
                warn!(
                    agent = self.config.name,
                    %id, error = %e,
                    "resume signal: resume_approved failed (non-fatal) — no reply"
                );
                return Ok(());
            }
        };

        match outcome {
            ThinkOutcome::Reply(text) => {
                // Reply target: same rule as the normal route — origin FIRST
                // (the suspended turn's `conversation_origin`), FALLBACK to the
                // agent's static reply target.
                let target: Option<&str> = reply_origin
                    .as_ref()
                    .map(familyclaw_bus::MessageOrigin::reply_target)
                    .or(self.reply_target.as_deref());
                let Some(target) = target else {
                    debug!(
                        agent = self.config.name,
                        %id,
                        "resume signal: no reply target (no origin, no static target) — dropping reply"
                    );
                    return Ok(());
                };
                match OutboundMessage::new(target, text) {
                    Ok(reply) => {
                        if let Err(e) = self.route_reply(reply) {
                            // A closed sink must not crash the actor — log and continue.
                            warn!(
                                agent = self.config.name,
                                %id, error = %e,
                                "resume signal: reply routing failed (non-fatal)"
                            );
                        }
                    }
                    Err(e) => warn!(
                        agent = self.config.name,
                        %id, error = %e,
                        "resume signal: reply build failed (non-fatal)"
                    ),
                }
            }
            ThinkOutcome::Suspended { approval_id, .. } => {
                // The continuation required a NEW approval (chained). This signal does
                // not grant it — no-op + info. The operator grants the next one separately.
                info!(
                    agent = self.config.name,
                    next_approval = %approval_id,
                    "resume signal: turn re-suspended awaiting further approval — no reply yet"
                );
            }
            ThinkOutcome::NoReply => {
                // No answer (e.g. max-iter or no LLM) — no-op.
            }
        }

        Ok(())
    }

    /// Handles a single turn **crash-resiliently** (without a per-message
    /// origin — uses the static reply target if set).
    ///
    /// This is a backward-compatible shell for
    /// [`handle_turn_with_origin`](Self::handle_turn_with_origin)
    /// with `origin = None`. The reply is routed to the agent's static
    /// [`with_reply_target`](Self::with_reply_target) target.
    ///
    /// # Errors
    /// - [`FamilyClawError::Memory`] if the memory write fails.
    /// - [`FamilyClawError`] (wrapped) if a durable step fails.
    pub async fn handle_turn(
        &mut self,
        sender: BeingId,
        message: &BusMessage,
    ) -> Result<TurnOutcome> {
        self.handle_turn_with_origin(sender, message, None).await
    }

    /// Handles a single turn **crash-resiliently**, with a per-message origin
    /// ([`familyclaw_bus::MessageOrigin`]) (F2).
    ///
    /// The turn's *outcome* ([`TurnOutcome`]) is recorded in a durable step
    /// ([`DurableContext::step`]), so on restart already-executed turns are
    /// replayed from the journal without running side effects again
    /// (design §2.1). The memory write itself is performed according to the
    /// flag inferred by the step.
    ///
    /// ## Deriving the reply target (F2 core)
    /// The reply target is derived **per message**: if `origin` is given, the target
    /// is `origin.reply_target()` (the conversation the message came from). Otherwise
    /// it falls back to the agent's static [`with_reply_target`](Self::with_reply_target)
    /// value. This lets a single agent serve many conversations without
    /// replies leaking to the wrong target — and does not break the single-channel +
    /// static-target MVP behavior (`origin = None` → the former path).
    ///
    /// Returns the turn's outcome.
    ///
    /// # Errors
    /// - [`FamilyClawError::Memory`] if the memory write fails.
    /// - [`FamilyClawError`] (wrapped) if a durable step fails.
    // Turn handling is a coherent, sequential process; splitting it up just
    // for the line count would fragment this logical unit.
    #[allow(clippy::too_many_lines)]
    pub async fn handle_turn_with_origin(
        &mut self,
        sender: BeingId,
        message: &BusMessage,
        origin: Option<&familyclaw_bus::MessageOrigin>,
    ) -> Result<TurnOutcome> {
        let turn = self.turn_counter;
        self.turn_user_reply_sent.store(false, Ordering::Relaxed);
        self.turn_reply_suppressed.store(false, Ordering::Relaxed);
        let step_name = format!("turn-{turn}");

        // 1. Deterministic, side-effect-free inference in a durable step:
        //    what should happen in this turn? On replay this is returned
        //    from the journal — we do not query the clock or randomness inside the closure.
        let summary = summarize(sender, message);
        let remembered = should_remember(message);
        let outcome = TurnOutcome {
            turn,
            remembered,
            summary,
        };

        // Deterministic inference: turn outcome ready.
        // Idempotent handling of the (memory) side effect follows below (step 2).
        let recorded: TurnOutcome = self
            .durable
            .step(&step_name, {
                let outcome = outcome.clone();
                move || Ok(outcome)
            })
            .map_err(|e| FamilyClawError::bus(format!("durable turn step failed: {e}")))?;

        // 2. The side effect (memory write) runs only on a FRESH turn, not on
        //    replay: the memory was already recorded in the original run. This way
        //    the side effect happens exactly once over the whole workflow's
        //    lifetime, even if the process crashes and turns are replayed from the journal.
        //
        //    We build the memory SYNCHRONOUSLY (borrowing `&self`) and then
        //    move only the needed owned values (the Arc memory handle + the
        //    finished memory) across the `.await` boundary. This way the async
        //    future does not capture an `&Agent` reference, and it stays `Send`
        //    (Ractor requires it).
        // Idempotent memory write: runs ALWAYS (also on replay), because
        // MemoryStore::add skips duplicates based on turn_key.
        // This resolves the dual-write problem: if durable.step succeeds
        // but the process crashes before the memory_store.add call,
        // on replay the memory is written again and the store ignores it.
        if recorded.remembered {
            let memory_store = Arc::clone(&self.memory);
            let mut memory = self.build_memory(sender, message, origin);
            memory.turn_key = Some(format!("{}:turn-{}", self.config.name, turn));
            memory_store
                .add(memory)
                .await
                .map_err(|e| FamilyClawError::memory(format!("remember failed: {e}")))?;
        }

        // 3. Update the emotional state based on the message (local, non-durable).
        self.apply_emotional_effect(message);

        // 4. Emotional homeostasis: after every turn the emotional state drifts
        //    slightly back toward neutral. This prevents exponential saturation
        //    (a feedback loop) in continuous sibling conversations: without
        //    dampening, CONTAGION_FACTOR piles up emotional states without bound
        //    and agents "burn out" within a few dozen turns.
        self.apply_emotional_homeostasis();

        // 5. LLM thinking (a side effect): if an LLM client is configured,
        //    the agent "thinks" based on the message. LLM generation is an EXTERNAL
        //    side effect → we run it in a fresh turn in the PROPER async
        //    context (not `block_on` inside a durable closure, which would
        //    panic on a `current_thread` runtime / could deadlock) and
        //    store the RESULT in a durable step. On replay we do not run
        //    `think` again — `durable.step` returns the stored text.
        //
        //    **Phase 1 governor filtering:** When a governor is installed AND
        //    the message is an `EmotionPulse` (siblings' "blood", not speech), it does
        //    NOT think at all. This prevents unnecessary LLM calls in affective
        //    pulse chains and ensures that only spoken messages produce an
        //    LLM answer. This is a deliberate fix for one of the most important
        //    Phase 1 gaps (on the pitfall list).
        let think_step = format!("{step_name}-think");
        let governor_filtered_pulse =
            self.governor.is_some() && matches!(message, BusMessage::EmotionPulse { .. });
        let governor_hesitate = self.governor.as_deref().is_some_and(|g| {
            let gov = EmotionActionGovernor::new(g);
            gov.decide(&self.emotion) == ActionDecision::Hesitate
        });
        if governor_filtered_pulse || governor_hesitate {
            self.turn_reply_suppressed.store(true, Ordering::Relaxed);
        }
        let will_think = self.llm.is_some() && !governor_filtered_pulse && !governor_hesitate;
        // Operator TOP20 bootstrap: deterministic file_write (no LLM theater).
        let operator_bootstrap_response = if will_think
            && !self.durable.is_replaying()
            && self.actions.is_some()
            && crate::identity::operator_execute_message(message, origin)
        {
            match self.execute_operator_top20_bootstrap(&step_name).await {
                Ok(reply) => Some(reply),
                Err(e) => {
                    warn!("operator bootstrap failed (non-fatal): {e}");
                    Some(crate::identity::operator_top20_bootstrap_blocked_reply(
                        &e.to_string(),
                    ))
                }
            }
        } else {
            None
        };
        let operator_status_response = if operator_bootstrap_response.is_none()
            && will_think
            && !self.durable.is_replaying()
            && crate::identity::operator_status_message(message, origin)
        {
            match self.execute_operator_status_bootstrap(&step_name) {
                Ok(reply) => Some(reply),
                Err(e) => {
                    warn!("operator status bootstrap failed (non-fatal): {e}");
                    Some(format!("STATUS ERROR: {e}"))
                }
            }
        } else {
            None
        };
        let operator_approval_response = if self.durable.is_replaying() {
            None
        } else {
            match self.handle_operator_approval_command(message, origin).await {
                Ok(reply) => reply,
                Err(e) => Some(format!("Approval command failed: {e}")),
            }
        };
        // Operator diagnostics fast path + brief ping fast path:
        // skip LLM when we can answer deterministically.
        let brief_ping_response = if operator_bootstrap_response.is_some() {
            operator_bootstrap_response
        } else if operator_approval_response.is_some() {
            operator_approval_response
        } else if operator_status_response.is_some() {
            operator_status_response
        } else if will_think && !self.durable.is_replaying() {
            let diag = crate::identity::operator_diagnostic_reply(message, origin);
            if diag.is_some() {
                diag
            } else {
                crate::identity::brief_ping_reply(&self.config.name, message)
            }
        } else {
            None
        };
        let will_think = will_think && brief_ping_response.is_none();
        let typing_abort = if !self.durable.is_replaying() && will_think {
            self.notify_turn_started(origin);
            self.spawn_typing_heartbeat(origin)
        } else {
            None
        };
        if let Ok(mut slot) = self.typing_abort.lock() {
            if let Some(old) = slot.take() {
                old.abort();
            }
            *slot = typing_abort;
        }
        // `thought_response` = the model's text reply (if `ThinkOutcome::Reply`),
        // `suspend` = the turn's suspension awaiting approval (if
        // `ThinkOutcome::Suspended`). These are mutually exclusive: one turn
        // produces at most one of them. `Suspended` does NOT go into the reply
        // pipeline — it is recorded in the turn's durable state for resume
        // (id + redacted summary), and is never routed to the user.
        let mut suspend: Option<(ApprovalId, String)> = None;
        let thought_response: Option<String> = if let Some(ref brief) = brief_ping_response {
            if !self.durable.is_replaying() {
                let think_step = format!("{step_name}-think");
                let _ = self.durable.step(&think_step, {
                    let brief = brief.clone();
                    move || Ok(brief)
                });
            }
            brief_ping_response
        } else if self.llm.is_none() {
            None
        } else if governor_filtered_pulse {
            // Phase 1: EmotionPulse = "blood", not thought. Log that this is
            // not run during replay, but think returns None on this turn.
            debug!(
                agent = self.config.name,
                "governor: skipping think() for EmotionPulse (filtered as 'blood', not speech)"
            );
            self.durable
                .step(&think_step, || Ok(String::new()))
                .ok()
                .filter(|s| !s.is_empty())
        } else if governor_hesitate {
            // Phase 1: safety veto (fear/anger/shame over the ceiling) → do
            // not think with the LLM on this turn. Log it.
            debug!(
                agent = self.config.name,
                "governor: Hesitate decision blocks think() (safety veto)"
            );
            self.durable
                .step(&think_step, || Ok(String::new()))
                .ok()
                .filter(|s| !s.is_empty())
        } else if self.actions.is_some() {
            // **Actions path (D1 durable tool-loop).** Run the loop journaled
            // per-dispatch over `&mut self.durable`. This handles BOTH a
            // fresh run AND a replay together: the loop checks
            // `is_replaying()` per dispatch, so dispatches already recorded
            // in a replayed turn are returned from the log (the side effect
            // does NOT repeat, the `SubmitOutcome`/ApprovalId/TTL value is
            // identical) and only once the log ends does it continue fresh.
            // The `-think`/`-suspend` step is closed inside the method.
            // Replaces the former "one `-think` step for the whole think"
            // model, which lost the turn's internal progress on a crash
            // between two dispatches (red-team finding).
            let (thought, susp) = match self.think_actions_durable(message, origin, turn).await {
                Ok(pair) => pair,
                Err(e) => {
                    warn!("think_actions_durable failed (non-fatal): {e}");
                    (Some(recovery_fallback_reply()), None)
                }
            };
            suspend = susp;
            thought.filter(|s| !s.is_empty())
        } else if self.durable.is_replaying() {
            // Replay (non-actions path): return the stored LLM reply from
            // the log without a new call.
            self.durable
                .step(&think_step, || Ok(String::new()))
                .ok()
                .filter(|s| !s.is_empty())
        } else {
            // Fresh turn (non-actions path): one LLM call, store the result
            // in the step. Origin is passed through so that a possible
            // suspend records the correct conversation origin for resume.
            match self.think_with_origin(message, origin).await {
                Ok(ThinkOutcome::Reply(text)) => self
                    .durable
                    .step(&think_step, {
                        let text = text.clone();
                        move || Ok(text)
                    })
                    .ok()
                    .filter(|s| !s.is_empty()),
                Ok(ThinkOutcome::Suspended {
                    approval_id,
                    redacted_summary,
                }) => {
                    // Suspend is a STATE: the turn was suspended awaiting
                    // approval. It does NOT go into the reply pipeline. Store
                    // a safe summary in the durable step ("{step}-suspend") so
                    // that resume (a later `approve`) and replay can find the
                    // suspension. The stored form is
                    // `"<approval_id>|<redacted_summary>"` — NOT the raw
                    // payload, NOT secrets (redacted_summary is already safe
                    // for the operator). No reply text is produced → None.
                    // (This branch is not actually reached in practice, since
                    // suspend requires the actions runtime, but it is kept
                    // for completeness.)
                    let suspend_step = format!("{step_name}-suspend");
                    let payload = format!("{approval_id}|{redacted_summary}");
                    if let Err(e) = self.durable.step(&suspend_step, {
                        let payload = payload.clone();
                        move || Ok(payload)
                    }) {
                        warn!("durable suspend step failed (non-fatal): {e}");
                    }
                    debug!(
                        agent = self.config.name,
                        %approval_id,
                        "turn suspended awaiting approval — recorded in durable turn, no user reply"
                    );
                    suspend = Some((approval_id, redacted_summary));
                    None
                }
                Ok(ThinkOutcome::NoReply) => None,
                Err(e) => {
                    warn!("think failed (non-fatal): {e}");
                    Some(recovery_fallback_reply())
                }
            }
        };

        self.clear_typing_heartbeat();

        // 5a½. Short-term memory: append the successful exchange (user → agent)
        //      to this conversation's history, so the NEXT turn sees the
        //      continuity. This is the "reply more than once" fix. Only on a
        //      fresh turn (`!is_replaying()`) — replay must not double-record —
        //      and only when the turn produced a genuine text reply (suspend/
        //      no-reply does not belong in the conversation history).
        if !self.durable.is_replaying() {
            if let Some(reply) = thought_response.as_ref() {
                let user_text = bus_message_text(message);
                let conv_key = self.conversation_key(origin);
                self.append_history(&conv_key, &user_text, reply);
                if let Some(origin) = origin {
                    if let Err(e) = self
                        .persist_chat_turn(origin, CHAT_USER_TAG, &user_text)
                        .await
                    {
                        warn!("chat user persist failed (non-fatal): {e}");
                    }
                    if let Err(e) = self
                        .persist_chat_turn(origin, CHAT_ASSISTANT_TAG, reply)
                        .await
                    {
                        warn!("chat assistant persist failed (non-fatal): {e}");
                    }
                }
                // Phase 2: fresh turn produced a reply → into the metric (turn counter).
                self.emit_metric(MetricEvent::TurnCompleted);
            }
        }

        // 5b. Reply path (C1 Model A, TASK C2): if `think()` produced text
        //     AND a reply sink + reply target is installed, push the reply
        //     OUT to the channel. This is a DIFFERENT path than bus
        //     publishing — the gateway owns the recv end and calls
        //     `Channel::send`. We do NOT publish to the bus (infinite-loop
        //     guard: a bus reply would trigger a new handle_turn). Run ONLY
        //     on a fresh turn, not during replay: sending to the outside
        //     world is a non-idempotency boundary (it would duplicate the
        //     message to the user), so replay must not repeat it.
        //
        //     **Phase 1 governor gatekeeper:** When the governor is installed
        //     AND the decision is `Hesitate`, do NOT reply at all. This is a
        //     critical safety net: a flooded agent (high fear/anger) cannot
        //     send a destructive reply before the situation settles. The
        //     same applies to the `Reflect` state (the agent thinks
        //     internally and does not speak, even if the LLM produced text).
        let reply_decision_blocks = self.governor.as_deref().and_then(|g| {
            let gov = EmotionActionGovernor::new(g);
            match gov.decide(&self.emotion) {
                ActionDecision::Hesitate | ActionDecision::Reflect => {
                    debug!(
                        agent = self.config.name,
                        "governor: Hesitate/Reflect decision blocks reply (silenced)"
                    );
                    self.turn_reply_suppressed.store(true, Ordering::Relaxed);
                    Some(())
                }
                _ => None,
            }
        });
        if !self.durable.is_replaying() && reply_decision_blocks.is_none() {
            let outbound_text: Option<String> = thought_response
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    suspend
                        .as_ref()
                        .map(|(id, summary)| suspended_approval_user_reply(*id, summary))
                });
            if let Some(thought) = outbound_text.as_deref() {
                // F2: derive the reply target per message. Origin FIRST (the
                // conversation the message came from), FALLBACK to the
                // static reply target. This way >1 conversation routes
                // correctly, and the single-channel + static-target MVP
                // behavior is preserved (origin = None).
                let target: Option<&str> = origin
                    .map(familyclaw_bus::MessageOrigin::reply_target)
                    .or(self.reply_target.as_deref());
                if let Some(target) = target {
                    match OutboundMessage::new(target, thought) {
                        Ok(reply) => {
                            if let Err(e) = self.route_reply(reply) {
                                // A routing failure (closed sink) must not
                                // bring down the turn — log and continue.
                                warn!("reply routing failed (non-fatal): {e}");
                            }
                        }
                        // Empty target/body is already rejected earlier; just to be safe.
                        Err(e) => warn!("reply build failed (non-fatal): {e}"),
                    }
                }
            }
        }

        // Attach the LLM thought summary OR the suspend marker to the turn
        // summary. Reply and Suspended are mutually exclusive: at most one
        // of them is `Some`. The suspend marker carries only the redacted,
        // operator-safe summary + the approval identifier — NOT the raw
        // payload and NOT secrets (resume/audit context).
        let recorded = match (thought_response, suspend) {
            (Some(thought), _) if !thought.is_empty() => {
                let snippet: String = thought.chars().take(160).collect();
                TurnOutcome {
                    summary: format!("{} | thought: {snippet}", recorded.summary),
                    ..recorded
                }
            }
            (_, Some((approval_id, redacted_summary))) => TurnOutcome {
                summary: format!(
                    "{} | suspended(approval={approval_id}): {redacted_summary}",
                    recorded.summary
                ),
                ..recorded
            },
            _ => recorded,
        };

        self.turn_counter += 1;

        // Introspection probe: mirror the final emotion state (contagion +
        // homeostasis already applied) into an optional observation handle,
        // so that an external observer sees the spawned agent's emotion
        // state after contagion over the bus. No-op if no handle is installed.
        if let Some(probe) = self.emotion_probe.as_ref() {
            match probe.lock() {
                Ok(mut guard) => *guard = self.emotion,
                // The lock can only be poisoned if the observer panicked
                // while holding it; mirror anyway rather than propagating
                // the panic into this turn.
                Err(poisoned) => *poisoned.into_inner() = self.emotion,
            }
        }

        Ok(recorded)
    }

    /// Builds a memory from a message according to the agent's current emotion state.
    ///
    /// Purely synchronous: it does not touch memory storage, so the caller
    /// can carry the finished [`Memory`] across an `.await` boundary without
    /// an `&self` borrow.
    fn build_memory(
        &self,
        sender: BeingId,
        message: &BusMessage,
        origin: Option<&MessageOrigin>,
    ) -> Memory {
        let content = match message {
            BusMessage::Text { body } => body.clone(),
            BusMessage::Latent { text_shadow, .. } => text_shadow.clone(),
            other => format!("[{}] from {sender}", other.kind_label()),
        };

        // Emotional tone and importance are derived from the agent's current
        // state — generically, not from hardcoded family calibration.
        let vad = self.emotion.to_vad();
        let emotional_charge = vad_magnitude(&vad);
        let factors = ImportanceFactors::new(emotional_charge, 0.0, 0.3, 0.0);

        // F4 session isolation: tag the memory with a session tag when a
        // session is set, so that [`Agent::think`]'s recall can filter
        // per-session. Without a session (None), only the `from:` tag →
        // shared scope (current behavior, backward-compatible).
        let mut tags = vec![format!("from:{sender}")];
        if let Some(origin) = origin {
            tags.push(crate::identity::peer_tag(&origin.sender));
            if crate::identity::is_operator_origin(Some(origin)) {
                tags.push(crate::identity::scope_operator_tag().to_string());
            }
        }
        if let Some(tag) = self.session_tag_for_recall(origin) {
            tags.push(tag);
        } else if let Some(origin) = self.session.as_ref() {
            tags.push(origin.session_tag());
        }
        let mut builder = Memory::builder(content)
            .vad(vad)
            .factors(factors)
            .decay_policy(DecayPolicy::Normal)
            .source("bus")
            .tags(tags);
        if let Some((dim, _)) = self.emotion.dominant() {
            builder = builder.emotions([dim]);
        }
        builder.build()
    }

    /// Applies the message's emotional effect to the agent's state.
    ///
    /// - **`EmotionPulse`** from a sibling → *affective contagion*:
    ///   the receiver adopts part of the sender's emotion state ([`CONTAGION_FACTOR`]).
    /// - **`Text`/`Latent`** → a light curiosity stimulus (contact is refreshing).
    fn apply_emotional_effect(&mut self, message: &BusMessage) {
        match message {
            BusMessage::EmotionPulse { state } => {
                for dim in Dimension::ALL {
                    // Affective contagion as *approach*, not accumulation:
                    // the receiver moves toward the source's emotion state
                    // by a fraction of CONTAGION_FACTOR. Because the delta is
                    // computed from the DIFFERENCE (source − receiver), the
                    // value can never exceed the source nor saturate at the
                    // ceiling — every pulse shrinks as the values approach
                    // each other. This fixes code review #2's "production
                    // crasher" bug, where the `source * CONTAGION_FACTOR`
                    // accumulation + 10% homeostasis balanced out at
                    // `2.25 * source` → saturation at the ceiling.
                    let current = self.emotion.value(dim);
                    let delta = (state.value(dim) - current) * CONTAGION_FACTOR;
                    self.emotion.stimulate(dim, delta);
                }
            }
            BusMessage::Text { .. } | BusMessage::Latent { .. } => {
                // Contact refreshes curiosity. The calibration's
                // `sensitivity` scales the stimulus strength (Layer B
                // tuning); with a neutral factor of 1.0 → the former +5.0.
                let sensitivity = self.calibration.sensitivity(Dimension::Curiosity);
                self.emotion
                    .stimulate(Dimension::Curiosity, 5.0 * sensitivity);
            }
            // Task and custom messages do not change the emotion state by default.
            _ => {}
        }
    }

    /// Emotional homeostasis: pulls each dimension slightly toward the
    /// calibration's **rest state** (`HOMEOSTASIS_RATE` * deviation from
    /// baseline, scaled by the dimension's `decay_rate` factor). This is the
    /// biological counterpart: emotional expression fades without a
    /// continuing cause.
    ///
    /// With neutral calibration `baseline = 0`, `decay_rate = 1` → the
    /// former behavior (e.g. `Joy = 80`, rest state 0, deviation 80,
    /// recovery `0.10 * 80 = 8`, new value `72`). Layer B's calibration can
    /// pull a dimension toward a non-zero rest value (e.g. the agent's
    /// baseline curiosity) and adjust the recovery rate (`decay_rate < 1` =
    /// the emotion "sticks").
    fn apply_emotional_homeostasis(&mut self) {
        for dim in Dimension::ALL {
            let current = self.emotion.value(dim);
            // Rest state from calibration (neutrally 0.0).
            let baseline = self.calibration.baseline(dim);
            let deviation = current - baseline;
            if deviation.abs() > 0.01 {
                // decay_rate scales the recovery rate (neutrally 1.0).
                let rate = self.calibration.decay_rate(dim).max(0.0);
                let correction = deviation * HOMEOSTASIS_RATE * rate;
                let new_value = current - correction;
                self.emotion.set(dim, new_value);
            }
        }
    }

    /// Publishes the agent's current emotion state as a pulse on the bus
    /// (affective nervous system): siblings sense it.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if publishing fails.
    pub fn broadcast_emotion(&self) -> Result<()> {
        self.bus
            .publish(self.being_id, BusMessage::emotion_pulse(self.emotion))
    }

    /// Forces a watchdog reply on the channel when the turn gets stuck or fails.
    pub fn force_watchdog_reply(&self, origin: Option<&MessageOrigin>, body: &str) -> Result<()> {
        let Some(target) = self.reply_target_for_origin(origin) else {
            return Ok(());
        };
        let reply = OutboundMessage::new(target, body)
            .map_err(|e| FamilyClawError::bus(format!("watchdog reply build failed: {e}")))?;
        self.route_reply(reply)
    }

    /// Turn watchdog: send a silence warning if the user message produced no reply.
    pub fn enforce_watchdog_after_turn(
        &self,
        message: &BusMessage,
        origin: Option<&MessageOrigin>,
    ) -> Result<()> {
        if !watchdog::message_expects_user_reply(message) {
            return Ok(());
        }
        if self.turn_reply_suppressed.load(Ordering::Relaxed)
            || self.turn_user_reply_sent.load(Ordering::Relaxed)
        {
            return Ok(());
        }
        if self.reply_target_for_origin(origin).is_none() {
            return Ok(());
        }
        warn!(
            agent = self.config.name,
            "turn-watchdog: user message produced no reply — sending fallback"
        );
        self.force_watchdog_reply(origin, watchdog::WATCHDOG_SILENCE_MSG)
    }

    /// Publishes a text message on the bus on the agent's behalf.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if publishing fails.
    pub fn say(&self, body: impl Into<String>) -> Result<()> {
        self.bus.publish(self.being_id, BusMessage::text(body))
    }

    /// Routes the reply **out to the channel** via the reply sink (C1 Model A).
    ///
    /// This is a **different path** than [`Agent::say`]/[`Agent::broadcast_emotion`]:
    /// those publish to the bus (siblings hear it), whereas `route_reply`
    /// pushes the message into an mpsc channel that the gateway owns and
    /// through which `Channel::send` is called out to the outside world.
    /// **No bus publish** — a bus reply would trigger a new
    /// [`Agent::handle_turn`] (infinite loop).
    ///
    /// No-op (returns `Ok`) if no reply sink is installed — this is the
    /// backward-compatible default behavior (replies are dropped).
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if a sink is installed but the receiving end
    /// is closed (the gateway stopped) — the reply could not be delivered.
    pub fn route_reply(&self, msg: OutboundMessage) -> Result<()> {
        if matches!(msg.kind, OutboundKind::Message) {
            self.turn_user_reply_sent.store(true, Ordering::Relaxed);
        }
        match self.reply_sink.as_ref() {
            Some(sink) => sink
                .send(msg)
                .map_err(|e| FamilyClawError::bus(format!("reply sink closed: {e}"))),
            // No sink = drop the reply (current behavior, backward-compatible).
            None => Ok(()),
        }
    }

    /// Returns this turn's reply target (origin first, then static fallback).
    fn reply_target_for_origin(&self, origin: Option<&MessageOrigin>) -> Option<String> {
        origin
            .map(MessageOrigin::reply_target)
            .map(str::to_owned)
            .or_else(|| self.reply_target.clone())
    }

    /// Cancels the active typing heartbeat (end of turn or watchdog).
    fn clear_typing_heartbeat(&self) {
        if let Ok(mut slot) = self.typing_abort.lock() {
            if let Some(handle) = slot.take() {
                handle.abort();
            }
        }
    }

    /// Immediately sends an ack message + typing indicator at the start of a long turn.
    fn notify_turn_started(&self, origin: Option<&MessageOrigin>) {
        let Some(target) = self.reply_target_for_origin(origin) else {
            return;
        };
        if should_emit_public_progress(origin) {
            if let Ok(ack) = OutboundMessage::progress(&target, "Working on it… ✦") {
                if let Err(e) = self.route_reply(ack) {
                    warn!("turn-start ack failed (non-fatal): {e}");
                }
            }
        }
        if let Ok(typing) = OutboundMessage::typing(&target) {
            if let Err(e) = self.route_reply(typing) {
                warn!("turn-start typing failed (non-fatal): {e}");
            }
        }
    }

    /// Keeps the Discord/Telegram typing indicator alive (~every 8 s).
    fn spawn_typing_heartbeat(
        &self,
        origin: Option<&MessageOrigin>,
    ) -> Option<tokio::task::AbortHandle> {
        let target = self.reply_target_for_origin(origin)?;
        let sink = self.reply_sink.clone()?;
        let handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                if let Ok(typing) = OutboundMessage::typing(&target) {
                    let _ = sink.send(typing);
                }
            }
        });
        Some(handle.abort_handle())
    }

    /// Publishes a task event on the bus (a light signal to siblings).
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if publishing fails.
    pub fn announce_task(&self, kind: TaskEventKind, task_id: impl Into<String>) -> Result<()> {
        self.bus
            .publish(self.being_id, BusMessage::task_event(kind, task_id))
    }

    /// Retrieves from the agent's memory with the given context (current time).
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] if the retrieval fails.
    pub async fn recall(&self, ctx: &RetrievalContext) -> Result<Vec<RetrievalResult>> {
        self.memory.retrieve(ctx, time::now()).await
    }

    /// Spawns the agent as a Ractor actor and **registers it on the bus**.
    ///
    /// Returns the actor reference ([`ActorRef`]). The being immediately
    /// starts receiving sibling messages; each message is handled by
    /// [`handle_turn`].
    ///
    /// [`handle_turn`]: Agent::handle_turn
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if starting the actor or registering it on
    /// the bus fails.
    pub async fn spawn(self) -> Result<ActorRef<ResonanceMessage>> {
        let name = self.config.name.clone();
        let being_id = self.being_id;
        let bus = self.bus.clone();

        // Spawned WITHOUT Ractor's global registration name (`None`):
        // being routing happens through the bus's own being registry
        // ([`BeingInfo`]), not Ractor's process-wide namespace. An
        // identically named agent (e.g. `agent_a` in two different
        // families/tests) then does not collide with a global
        // "already registered" error.
        let (actor, _join) = Actor::spawn(None, AgentActor::new(), self)
            .await
            .map_err(|e| FamilyClawError::bus(format!("agent '{name}' spawn failed: {e}")))?;

        // Register on the bus so siblings find the being and messages
        // are delivered to this mailbox.
        bus.register(BeingInfo::new(being_id, name, actor.clone()))?;
        Ok(actor)
    }
}

/// Derives the VAD coordinate's "intensity" (`0.0..=1.0`): how charged the
/// emotion state is. Used as the memory's emotion factor.
fn vad_magnitude(vad: &familyclaw_emotion::Vad) -> f32 {
    // Valence is -1..=1, arousal/dominance 0..=1. Absolute values are weighted.
    let v = vad.valence.abs();
    let a = vad.arousal;
    // Distance from neutral dominance (0.5).
    let d = (vad.dominance - 0.5).abs() * 2.0;
    ((v + a + d) / 3.0).clamp(0.0, 1.0)
}

/// Extracts the message's text representation for short-term memory. Same
/// logic as [`Agent::build_think_context`]'s `query` extraction, so that the
/// user text recorded in history matches what was sent to the model.
fn bus_message_text(message: &BusMessage) -> String {
    match message {
        BusMessage::Text { body } => body.clone(),
        BusMessage::Latent { text_shadow, .. } => text_shadow.clone(),
        other => format!("[{}]", other.kind_label()),
    }
}

/// Session tag per-message from [`MessageOrigin`] (F4, same format as `session.rs`).
fn session_tag_from_origin(origin: &MessageOrigin) -> String {
    format!("session:{}:{}", origin.channel_id, origin.conversation)
}

/// Generic user-visible fallback reply when the LLM/tool loop produces no text.
fn recovery_fallback_reply() -> String {
    "Anteeksi — en saanut vietyä pyyntöä loppuun (työkalu epäonnistui tai turvaraja täyttyi). \
     Yritä uudelleen tai kerro tarkemmin mitä tarvitset."
        .to_string()
}

/// Generic progress report derived from the tool name (OpenClaw/Hermes style).
fn tool_progress_label(tool_name: &str) -> String {
    let action = match tool_name {
        n if n.contains("file_write") => "Writing files",
        n if n.contains("file_patch") => "Applying patch",
        n if n.contains("fs_read") => "Reading files",
        n if n.contains("web_search") || n.contains("research") => "Searching the web",
        n if n.contains("web_fetch") => "Fetching a page",
        n if n.contains("github") => "Working with GitHub",
        n if n.contains("email") || n.contains("discord") => "Calling an integration",
        _ => "Running a tool",
    };
    action.to_string()
}

/// Public progress messages are kept on, so the user sees the progress.
fn should_emit_public_progress(origin: Option<&MessageOrigin>) -> bool {
    let _ = origin;
    true
}

const MAX_PROGRESS_PER_TURN: u32 = 5;
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_secs(4);
const TOOL_BUDGET_PER_NAME: u32 = 3;
const TOOL_BUDGET_FS_READ: u32 = 8;

struct ProgressGate {
    sent: u32,
    last_at: Option<Instant>,
}

impl ProgressGate {
    fn new() -> Self {
        Self {
            sent: 0,
            last_at: None,
        }
    }

    fn allow(&self) -> bool {
        if self.sent >= MAX_PROGRESS_PER_TURN {
            return false;
        }
        if let Some(last) = self.last_at {
            if Instant::now().duration_since(last) < PROGRESS_MIN_INTERVAL {
                return false;
            }
        }
        true
    }

    fn record(&mut self) {
        self.sent += 1;
        self.last_at = Some(Instant::now());
    }
}

/// User-visible notice when the turn is left waiting for a rare approval.
fn suspended_approval_user_reply(approval_id: ApprovalId, redacted_summary: &str) -> String {
    format!(
        "BLOCKED (hyväksyntä): {redacted_summary}\n\
         ID: `{approval_id}`\n\
         Operaattori Discordissa: `APPROVE {approval_id}` tai `DENY {approval_id}`\n\
         Tai gateway: POST /approvals/{approval_id}/approve"
    )
}

/// Formats a tool error into clear SYSTEM feedback for the model (anti-silence).
fn format_tool_failure_for_model(tool_name: &str, error: &impl std::fmt::Display) -> String {
    format!(
        "SYSTEM: Your previous action '{tool_name}' failed with error: {error}. \
         Acknowledge this failure to the user, explain what went wrong in plain language, \
         and suggest a corrected approach. Do not silently stop."
    )
}

/// One LLM call without tools after a stall situation.
async fn recover_user_visible_reply(llm: &LlmFailover, messages: &[LlmMessage]) -> Option<String> {
    let mut recovery_messages = messages.to_vec();
    recovery_messages.push(LlmMessage::user(
        "SYSTEM: Your previous tool calls failed or the turn hit the iteration limit. \
         Reply to the user in plain language: acknowledge the failure, summarize errors \
         from the tool results above, and suggest next steps. Do not call more tools.",
    ));
    match llm.complete(&recovery_messages).await {
        Ok(text) if !text.trim().is_empty() => Some(text),
        Ok(_) => None,
        Err(e) => {
            warn!("recovery LLM call failed (non-fatal): {e}");
            None
        }
    }
}

/// Builds the LLM message stack: `[system, ...history (oldest→newest), current_user]`.
///
/// `history` is the conversation's earlier user/assistant messages from a
/// sliding window (see [`Agent::history_for`]). This is the fix that gives
/// the model conversation continuity — without it the stack was always just
/// `[system, user]` and the agent "replied only once" (it did not see the
/// previous exchange).
fn build_message_stack(
    system_prompt: String,
    history: &[LlmMessage],
    query: String,
) -> Vec<LlmMessage> {
    let mut messages = Vec::with_capacity(history.len() + 2);
    messages.push(LlmMessage::system(system_prompt));
    messages.extend(history.iter().cloned());
    messages.push(LlmMessage::user(query));
    messages
}

/// Truncates the text to [`history_max_chars_per_msg`] (default
/// [`HISTORY_MAX_CHARS_PER_MSG`], overridable via
/// `FAMILYCLAW_HISTORY_MAX_CHARS`) respecting the UTF-8 boundary, for storage
/// in short-term memory. Short texts are returned as-is.
fn truncate_for_history(text: &str) -> String {
    let max = history_max_chars_per_msg();
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Deterministically decides whether a message is worth remembering.
///
/// Generic baseline rule: text and latent messages are remembered (they are
/// content between beings), emotion pulses and light task signals are not
/// (they are transient nervous-system "blood", not content).
fn should_remember(message: &BusMessage) -> bool {
    matches!(message, BusMessage::Text { .. } | BusMessage::Latent { .. })
}

/// Builds a short, deterministic summary of the turn (into the durable log).
fn summarize(sender: BeingId, message: &BusMessage) -> String {
    format!("{} from {sender}", message.kind_label())
}

/// Maps the MCP tool descriptors published by the runtime into
/// [`ToolDefinition`]s to be offered to the LLM (tool loop, Phase 1 keystone).
///
/// Only **valid** definitions ([`ToolDefinition::validate`]) are offered to
/// the model — an invalid name or a non-object schema is skipped and logged
/// at debug level, so that one invalid skill does not bring down the whole
/// call. `name`, `description`, and `input_schema` (→ `function.parameters`)
/// come directly from the descriptor; the required permission / trust level
/// is the actions layer's responsibility and is not part of the form
/// offered to the LLM.
fn build_tool_definitions(descriptors: &[McpToolDescriptor]) -> Vec<ToolDefinition> {
    descriptors
        .iter()
        .filter_map(|d| {
            let def = ToolDefinition {
                name: d.name.clone(),
                description: d.description.clone(),
                input_schema: d.input_schema.clone(),
            };
            match def.validate() {
                Ok(()) => Some(def),
                Err(e) => {
                    debug!(tool = d.name.as_str(), error = %e, "tool loop: skipping invalid tool definition");
                    None
                }
            }
        })
        .collect()
}

/// Redacts the message stack **for a resumable turn** before persisting it
/// to disk (suspend/resume bridge, secrets invariant).
///
/// Because a resumable turn is persisted to disk, the **entire** secrets
/// surface of **every** message must be redacted — not just tool-call
/// arguments, but also the messages' text content (`content`), in which a
/// secret can hide as free text. The previous version only redacted
/// `tool_calls` arguments, and only at the "whole value / known key name"
/// level, so that (a) system/user/assistant messages' `content` and (b) a
/// secret **embedded** in the model-produced argument's free text could
/// reach disk in raw form. This function closes both gaps:
///
/// - **Messages' `content`** is run through
///   [`familyclaw_actions::redact_free_text`] (a substring pass: individual
///   secret words + `Bearer …` + `key=value`). Tool messages' content is
///   already redacted in the actions pipeline (`proof.redacted_output`), but
///   this pass is idempotent and acts as defense in depth for
///   system/user/assistant texts too.
/// - **Tool calls' `arguments`** ([`crate::llm::ToolCall::arguments`]) is raw
///   JSON produced by the model. It is run through the **deep** redactor
///   ([`familyclaw_actions::redact_value_deep`]), which redacts both the
///   whole value / known key name AND secrets embedded in free text.
///
/// This is safe with respect to resume: the approved action has already been
/// executed (the payload is bound to the pending approval in the actions
/// layer), so the replayed assistant message only needs the tool call's
/// **id and name** to bind the `tool_result` to the right call — not the raw
/// arguments.
///
/// Returns a redacted copy (the original live stack is not mutated).
fn redact_messages_for_resume(messages: &[LlmMessage]) -> Vec<LlmMessage> {
    messages
        .iter()
        .map(|m| {
            // 1. Text content: redact secrets embedded in free text from
            //    every message (system/user/assistant/tool).
            let (redacted_content, _) = familyclaw_actions::redact_free_text(&m.content);
            // 2. Tool calls' arguments: deep redaction (incl. embedded ones).
            let redacted_calls = m.tool_calls.as_ref().map(|calls| {
                calls
                    .iter()
                    .map(|c| {
                        let (redacted_args, _) =
                            familyclaw_actions::redact_value_deep(&c.arguments);
                        crate::llm::ToolCall {
                            id: c.id.clone(),
                            name: c.name.clone(),
                            arguments: redacted_args,
                        }
                    })
                    .collect()
            });
            LlmMessage {
                content: redacted_content,
                tool_calls: redacted_calls,
                ..m.clone()
            }
        })
        .collect()
}

/// Derives the text to feed back to the model from a tool call's result
/// (tool loop, Phase 1 keystone).
///
/// Uses the task's **redacted proof bundle** if one was produced
/// ([`ActionRuntime::proof`]): the proof's `redacted_output` (secrets
/// removed) is serialized to JSON text, prefixed with a short summary. If
/// there is no proof (e.g. the task did not produce one), only a status
/// description is returned. The output never contains a raw secret — the
/// proof is already redacted in the actions pipeline.
fn tool_result_text(runtime: &ActionRuntime, submit: &familyclaw_actions::SubmitOutcome) -> String {
    tool_result_text_for(runtime, submit.task_id, submit.status)
}

/// Like [`tool_result_text`], but takes the task's id and status directly
/// (the durable-journaled dispatch path, where the whole [`SubmitOutcome`]
/// is not kept alive but [`DispatchRecord`] carries the id and status).
///
/// Same redaction guarantee: the proof is fetched by id
/// ([`ActionRuntime::proof`]) and its `redacted_output` is already
/// secret-free. If no proof is found (e.g. a replay branch where the
/// executor was not run again), only a status description is returned — the
/// output never contains a raw secret.
fn tool_result_text_for(
    runtime: &ActionRuntime,
    task_id: ActionTaskId,
    status: familyclaw_actions::task::TaskStatus,
) -> String {
    let failed = matches!(status, familyclaw_actions::task::TaskStatus::Failed);
    if let Some(proof) = runtime.proof(task_id) {
        let body =
            serde_json::to_string(&proof.redacted_output).unwrap_or_else(|_| "{}".to_string());
        let prefix = if failed { "FAILED: " } else { "" };
        format!(
            "{prefix}status={status:?}; {}; output={body}",
            proof.output_summary
        )
    } else if failed {
        format!("FAILED: status={status:?}; action did not succeed (no proof bundle)")
    } else {
        format!("status={status:?}; no proof produced")
    }
}

/// **Durable-journaled dispatch outcome** (D1, crash-replay).
///
/// Deliberately kept small and **deterministically serializable**: exactly
/// the part of [`SubmitOutcome`] that the tool loop needs to continue
/// (`task_id`, `status`, `pending_approval`). When the dispatch is journaled
/// ([`Agent::drive_tool_loop_durable`]), replay returns exactly this value —
/// including the random [`ApprovalId`] drawn at dispatch time and the
/// clock-derived TTL, which live on in the approval granted by
/// `submit_task` — **without running the skill's executor again**. This way
/// the side effect happens exactly once and the dispatch outcome is
/// value-identical to the original run.
///
/// A `submit_task` error (e.g. an unknown skill) is stored in
/// [`DispatchRecord::error`], so that it too replays identically and does
/// not run the executor again.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct DispatchRecord {
    /// The dispatched task's id (proof lookup + diagnostics).
    task_id: ActionTaskId,
    /// The task's status after the pipeline ran.
    status: familyclaw_actions::task::TaskStatus,
    /// The approval id if the dispatch was left waiting for approval.
    pending_approval: Option<ApprovalId>,
    /// `submit_task`'s error message if the dispatch failed (otherwise `None`).
    error: Option<String>,
}

impl DispatchRecord {
    /// Builds the journaled record from `submit_task`'s outcome.
    ///
    /// On success, copies the id, status, and any approval; on failure,
    /// stores the error message (nil id + redacted status), so that replay
    /// returns the same error without running the executor again.
    fn from_outcome(
        outcome: &familyclaw_actions::Result<familyclaw_actions::SubmitOutcome>,
    ) -> Self {
        match outcome {
            Ok(submit) => Self {
                task_id: submit.task_id,
                status: submit.status,
                pending_approval: submit.pending_approval,
                error: None,
            },
            Err(e) => Self {
                task_id: ActionTaskId::nil(),
                status: familyclaw_actions::task::TaskStatus::Failed,
                pending_approval: None,
                error: Some(e.to_string()),
            },
        }
    }
}

/// Records a single turn-audit event via a free function (durable dispatch
/// path, which cannot call an `&self` method for disjoint-borrow reasons).
///
/// Same **secrets invariant** as [`Agent::record_turn_audit`]: `detail` is
/// run through [`familyclaw_actions::redact_free_text`] before storage
/// (defense in depth). `at` (D1) is injected — the clock is not read here.
/// No-op when no collector is wired up (`audit = None`).
fn record_turn_audit_into(
    audit: Option<&Arc<AuditCollector>>,
    turn_id: ActionId,
    kind: AuditKind,
    at: Timestamp,
    detail: impl Into<String>,
) {
    let Some(audit) = audit else {
        return;
    };
    let (safe_detail, _) = familyclaw_actions::redact_free_text(&detail.into());
    audit.record(ExecAuditEvent::new(kind, turn_id, at, safe_detail));
}

/// Awaits `fut` in two stages against a soft/hard watchdog deadline pair: up
/// to `soft_secs`, then — if not finished — invokes `on_soft_deadline` once
/// and keeps awaiting the **same** future for the remaining time up to
/// `hard_secs`. Only past `hard_secs` is `fut` actually abandoned
/// (`Err(())`); a completion anywhere in between is delivered as `Ok(value)`
/// (late, but not discarded).
///
/// `fut` is heap-pinned (`Pin<Box<F>>`, not the stack-pinning `tokio::pin!`)
/// specifically so it is a plain, ownable value that can be `drop`ped
/// explicitly on every exit path. Dropping it is what releases whatever it
/// borrowed for its lifetime — in the caller below, `agent`'s exclusive
/// borrow via `handle_turn_with_origin(&mut self, ...)` — mirroring the
/// implicit drop that the old single-stage `tokio::time::timeout(...).await`
/// performed when it elapsed. The difference is that here, that drop only
/// happens at the hard cap: hitting the soft deadline no longer discards any
/// in-flight work.
async fn watchdog_two_stage<F, T>(
    mut fut: Pin<Box<F>>,
    soft_secs: u64,
    hard_secs: u64,
    on_soft_deadline: impl FnOnce(),
) -> std::result::Result<T, ()>
where
    F: Future<Output = T>,
{
    if let Ok(value) = tokio::time::timeout(Duration::from_secs(soft_secs), &mut fut).await {
        drop(fut);
        return Ok(value);
    }
    on_soft_deadline();
    let remaining = hard_secs.saturating_sub(soft_secs);
    let result = tokio::time::timeout(Duration::from_secs(remaining), &mut fut).await;
    drop(fut);
    result.map_err(|_| ())
}

/// Sends the watchdog "still working" interim notice directly through a
/// pre-cloned reply sink + pre-resolved reply target, **without** going
/// through [`Agent::force_watchdog_reply`]. This is required because at the
/// point this fires, the turn future in [`AgentActor::handle`] still holds
/// `agent`'s exclusive borrow (`handle_turn_with_origin(&mut self, ...)`) —
/// no method on `agent` can be called until that future is dropped. Mirrors
/// `force_watchdog_reply`'s target-resolution + sink-send exactly, just
/// using values captured before the borrow started.
fn send_watchdog_notice(sink: Option<&ReplySink>, target: Option<&str>, body: &str) {
    let (Some(sink), Some(target)) = (sink, target) else {
        return;
    };
    match OutboundMessage::new(target, body) {
        Ok(msg) => {
            let _ = sink.send(msg);
        }
        Err(e) => {
            warn!(error = %e, "watchdog still-working notice build failed");
        }
    }
}

/// Type-erased agent for actor (no generics).
type ErasedAgent = Agent;

/// [`Agent`]'s Ractor actor shell.
///
/// The state is [`Agent`] itself. The message type is [`ResonanceMessage`]
/// (the bus's language), so the actor connects to the bus through the same
/// interface as any being.
///
/// The actor is stateless (all state lives in the [`Agent`] value).
pub struct AgentActor {
    _marker: std::marker::PhantomData<fn() -> ErasedAgent>,
}

impl AgentActor {
    /// Builds a new (stateless) actor shell.
    #[must_use]
    fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl Actor for AgentActor {
    type Msg = ResonanceMessage;
    type State = ErasedAgent;
    type Arguments = ErasedAgent;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        agent: Self::Arguments,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        debug!(agent = agent.name(), being = %agent.being_id(), "agentti käynnistyy");
        Ok(agent)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        envelope: Self::Msg,
        agent: &mut Self::State,
    ) -> std::result::Result<(), ActorProcessingErr> {
        let sender = envelope.from;
        // Do not process our own echoes (the bus does not send them, but
        // just to be safe — hearing yourself is not a turn).
        if sender == agent.being_id {
            return Ok(());
        }

        // TASK 1: RESUME CONTROL SIGNAL before normal turn routing.
        // `ResumeApproval` is NOT a conversation but a control signal: it is
        // routed directly to the resume path (`handle_resume_signal` →
        // `resume_approved` + `route_reply`) and does NOT start a new LLM
        // turn (`handle_turn_with_origin`). The self-echo guard above
        // applies here too.
        if let BusMessage::ResumeApproval { approval_id } = &envelope.payload {
            if let Err(err) = agent.handle_resume_signal(approval_id, time::now()).await {
                // `handle_resume_signal` already handles errors fail-closed
                // (always returns Ok); this branch is defense in depth in
                // case the contract ever changes. One signal's error does
                // not bring down the being.
                warn!(agent = agent.name(), error = %err, "resume-signaalin käsittely epäonnistui");
            }
            return Ok(());
        }

        // F2: per-message origin from the envelope → the reply target is
        // derived per message (origin.reply_target()), fallback to the
        // static target.
        let origin = envelope.origin.clone();
        let payload = envelope.payload.clone();
        let watchdog_secs = watchdog::turn_watchdog_secs();
        let hard_secs = watchdog::turn_watchdog_hard_secs(watchdog_secs);

        // Precompute the reply route now: once `turn_future` below captures
        // `agent`'s exclusive borrow for the turn's lifetime (via
        // `handle_turn_with_origin(&mut self, ...)`), `agent` cannot be
        // touched again — not even for a shared read — until that future is
        // dropped. An interim notice sent *while* the turn is still running
        // therefore has to use values resolved up front, not `agent` itself.
        let notice_sink = agent.reply_sink.clone();
        let notice_target = agent.reply_target_for_origin(origin.as_ref());
        let agent_label = agent.name().to_string();

        let turn_future =
            Box::pin(agent.handle_turn_with_origin(sender, &payload, origin.as_ref()));
        // Soft deadline (`watchdog_secs`): send an interim "still working"
        // notice but keep awaiting the same future. Hard cap (`hard_secs`):
        // give up for good — this is the only point work is now discarded,
        // vs. the old behavior of dropping at the soft deadline every time.
        let turn_result = watchdog_two_stage(turn_future, watchdog_secs, hard_secs, || {
            warn!(
                agent = agent_label.as_str(),
                soft_secs = watchdog_secs,
                hard_secs,
                "turn-watchdog: soft deadline reached, turn still running — sending interim notice"
            );
            send_watchdog_notice(
                notice_sink.as_ref(),
                notice_target.as_deref(),
                &watchdog::watchdog_still_working_msg(hard_secs),
            );
        })
        .await;

        match turn_result {
            Ok(Ok(outcome)) => {
                debug!(
                    agent = agent.name(),
                    turn = outcome.turn,
                    remembered = outcome.remembered,
                    "vuoro käsitelty"
                );
                if let Err(err) = agent.enforce_watchdog_after_turn(&payload, origin.as_ref()) {
                    warn!(agent = agent.name(), error = %err, "turn-watchdog silence fallback failed");
                }
            }
            Ok(Err(err)) => {
                warn!(agent = agent.name(), error = %err, "vuoron käsittely epäonnistui");
                if let Err(e) =
                    agent.force_watchdog_reply(origin.as_ref(), watchdog::WATCHDOG_ERROR_MSG)
                {
                    warn!(agent = agent.name(), error = %e, "turn-watchdog error reply failed");
                }
            }
            Err(()) => {
                agent.clear_typing_heartbeat();
                warn!(
                    agent = agent.name(),
                    soft_secs = watchdog_secs,
                    hard_secs,
                    "turn-watchdog: vuoro ylitti kovan aikarajan"
                );
                if let Err(e) =
                    agent.force_watchdog_reply(origin.as_ref(), watchdog::WATCHDOG_TIMEOUT_MSG)
                {
                    warn!(agent = agent.name(), error = %e, "turn-watchdog timeout reply failed");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Emotion state values flow as exact f32 constants — exact comparison is fine.
    #![allow(clippy::float_cmp)]

    use super::*;
    use familyclaw_bus::ResonanceBus;
    use familyclaw_core::ModelConfig;
    use familyclaw_durable::InMemoryJournal;
    use familyclaw_memory::LocalJsonStore;

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
            let durable = DurableContext::new(
                Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>
            )
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
        let scoped =
            RetrievalContext::new("sessio-viesti").with_required_tags([origin.session_tag()]);
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
            let durable = DurableContext::new(
                Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>
            )
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
        let b_scope = RetrievalContext::new("salaisuus kanavasta A")
            .with_required_tags([origin_b.session_tag()]);
        let b_sees = agent_b.recall(&b_scope).await.expect("recall b");
        assert!(
            b_sees
                .iter()
                .all(|r| !r.memory.content.contains("kanavasta A")),
            "B:n sessio ei saa nähdä A:n muistoa"
        );

        // A's session scope sees A's own memory (positive control).
        let a_scope = RetrievalContext::new("salaisuus kanavasta A")
            .with_required_tags([origin_a.session_tag()]);
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
        let agent =
            agent_with_scripted_llm("agent_a", bus.clone(), &api).with_actions(echo_runtime());
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
        let agent =
            agent_with_scripted_llm("agent_a", bus.clone(), &api).with_actions(echo_runtime());

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
        let agent =
            agent_with_scripted_llm("agent_a", bus.clone(), &api).with_actions(echo_runtime());

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
        let agent =
            agent_with_scripted_llm("agent_a", bus.clone(), &api).with_actions(echo_runtime());
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
    use familyclaw_actions::{DangerousToolRateLimiter, JournalPendingStore, PendingApprovalStore};

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
            let resumable: StdArc<dyn ResumableTurnStore> = StdArc::new(
                JournalResumableStore::open(dir.resumable_path()).expect("resumable 1"),
            );
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
            let mut agent = agent_over_file_journal(
                "agent_a",
                bus.clone(),
                &api,
                &journal_path,
                StdArc::clone(&memory),
            )
            .with_actions(StdArc::clone(&runtime));

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
            let mut agent = agent_over_file_journal(
                "agent_a",
                bus.clone(),
                &api,
                &journal_path,
                StdArc::clone(&memory),
            )
            .with_actions(StdArc::clone(&runtime));
            // The context is in replay mode (the log has the earlier turn's steps).
            assert!(
                agent.durable.is_replaying(),
                "rebuild näkee aiemman vuoron askeleet → replay-tila"
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
            let mut agent = agent_over_file_journal(
                "agent_a",
                bus.clone(),
                &api,
                &journal_path,
                StdArc::clone(&memory),
            )
            .with_actions(StdArc::clone(&runtime));
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
                let resumable: StdArc<dyn ResumableTurnStore> = StdArc::new(
                    JournalResumableStore::open(&resumable_path).expect("resumable open"),
                );
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
            assert_eq!(body.len(), 500, "truncate_for_history must respect the env override");
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
}
