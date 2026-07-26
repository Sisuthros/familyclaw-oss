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

mod actor;
mod helpers;
mod metrics;
mod prelude;
mod turn;

use prelude::*;

#[cfg(test)]
mod tests;

pub use actor::AgentActor;

use helpers::{should_emit_public_progress, vad_magnitude};

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
pub(super) const CONTAGION_FACTOR: f32 = 0.25;

/// Default TTL in minutes for a resumable turn, when the pending approval's
/// expiry moment cannot be obtained from [`ActionRuntime`] for some reason
/// (e.g. the approval has already been evicted). Kept equal to the actions
/// layer's `DEFAULT_APPROVAL_TTL_MINUTES` so a resumable turn never outlives
/// the approval. In practice the expiry is derived directly from the pending
/// approval ([`ActionRuntime::pending_expiry_for`]); this is used only as a
/// fallback.
pub(super) const RESUMABLE_DEFAULT_TTL_MINUTES: i64 = 60;

/// After each turn, the emotion state recovers toward neutral by this
/// percentage. A value of 0.10 (10%) means: after 10 turns of continuous
/// sibling influence, the emotion state is less than half of its maximum
/// (exponential decay). This prevents feedback-loop saturation.
pub(super) const HOMEOSTASIS_RATE: f32 = 0.10;

/// How many CONSECUTIVE history messages (user+assistant) are kept per
/// conversation for LLM context. 20 = ~10 turn pairs: enough for continuity
/// of a Discord conversation without bloating the context. The oldest is
/// dropped once the cap is exceeded (sliding window).
pub(crate) const HISTORY_MAX_MESSAGES: usize = 20;

/// Character cap for a single history message. A long message is truncated
/// to this before being saved to history — prevents one giant message from
/// consuming the entire window. Default; overridable via
/// `FAMILYCLAW_HISTORY_MAX_CHARS` (see [`history_max_chars_per_msg`]) — a
/// low default here stunts the agent's memory of its own longer replies once
/// [`crate::llm::DEFAULT_MAX_TOKENS`] is raised, so deployments generating
/// long replies routinely can raise this too.
pub(crate) const HISTORY_MAX_CHARS_PER_MSG: usize = 1500;

/// Minimum accepted value for `FAMILYCLAW_HISTORY_MAX_CHARS` — guards against
/// a misconfigured tiny value truncating history into uselessness.
pub(crate) const HISTORY_MAX_CHARS_MIN: usize = 200;

/// Reads the `FAMILYCLAW_HISTORY_MAX_CHARS` environment variable, or returns
/// [`HISTORY_MAX_CHARS_PER_MSG`]. Follows the same env-var-reader shape as
/// [`crate::watchdog::turn_watchdog_secs`]: parse, filter to a valid range,
/// default on anything else (missing, unparseable, or below
/// [`HISTORY_MAX_CHARS_MIN`]).
#[must_use]
pub(crate) fn history_max_chars_per_msg() -> usize {
    std::env::var("FAMILYCLAW_HISTORY_MAX_CHARS")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|&n: &usize| n >= HISTORY_MAX_CHARS_MIN)
        .unwrap_or(HISTORY_MAX_CHARS_PER_MSG)
}

/// Type of an optional **summarizer model hook** for
/// [`CompactionConfig`]: given the messages about to be evicted
/// (oldest→newest, already excluding the protected head/tail), returns
/// either a single synthetic [`LlmMessage`] that replaces the whole
/// evicted zone (`Some`), or `None` to decline and fall back to plain
/// oldest-first eviction for this round. Synchronous and side-effect-free
/// by contract — [`compact_history`] calls it inline while holding the
/// history lock; a real summarizer implementation should keep its own
/// (e.g. blocking-LLM-call) work fast or pre-computed, the same
/// constraint [`familyclaw_actions::redact_free_text`] callers already
/// live under.
pub type CompactionSummarizer = Arc<dyn Fn(&[LlmMessage]) -> Option<LlmMessage> + Send + Sync>;

/// **Configurable context-compaction policy** (Hermes-style context
/// management) for [`Agent::append_history`] / [`compact_history`].
///
/// Generalizes the old hardcoded "drop the oldest message once the window
/// exceeds [`HISTORY_MAX_MESSAGES`]" rule into three independent knobs:
/// - [`max_messages`](Self::max_messages) — the threshold that triggers
///   compaction (was the hardcoded [`HISTORY_MAX_MESSAGES`]).
/// - [`protect_first_n`](Self::protect_first_n) — pins the conversation's
///   opening N messages (e.g. a scene-setting first exchange) so
///   compaction never evicts them, no matter how long the conversation
///   grows. `0` (default) = no protected head, matching the old behavior.
/// - [`protect_last_n`](Self::protect_last_n) — pins the most recent N
///   messages. Defaults to [`Self::DEFAULT_MAX_MESSAGES`], which
///   reproduces the old plain sliding window (everything old enough to be
///   evicted is exactly the single oldest message).
/// - [`summarizer`](Self::summarizer) — optional hook: when there is a
///   non-trivial "middle" zone to evict (between the protected head and
///   tail), the WHOLE zone is handed to the summarizer at once. Returning
///   `Some(message)` collapses it into one synthetic message instead of
///   dropping it outright; `None` (the default, and also what the
///   summarizer may return to decline) falls back to plain eviction.
///
/// Not `Debug`/`PartialEq`/`Copy` — [`summarizer`](Self::summarizer) is a
/// closure trait object, unlike [`ToolLoopConfig`].
#[derive(Clone)]
pub struct CompactionConfig {
    /// Compact once the history exceeds this many messages
    /// (user+assistant together). `0` disables compaction entirely (the
    /// window then grows without bound — manual/external management
    /// only, mirrors `pending_store`'s `factor = 0` convention).
    pub max_messages: usize,
    /// How many of the OLDEST messages are pinned and never evicted.
    pub protect_first_n: usize,
    /// How many of the MOST RECENT messages are pinned and never evicted.
    pub protect_last_n: usize,
    /// Optional summarizer model hook — see [`CompactionSummarizer`].
    pub summarizer: Option<CompactionSummarizer>,
}

impl std::fmt::Debug for CompactionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompactionConfig")
            .field("max_messages", &self.max_messages)
            .field("protect_first_n", &self.protect_first_n)
            .field("protect_last_n", &self.protect_last_n)
            .field("summarizer", &self.summarizer.is_some())
            .finish()
    }
}

impl CompactionConfig {
    /// Default message-count threshold — identical to the previous
    /// hardcoded [`HISTORY_MAX_MESSAGES`], so [`CompactionConfig::default`]
    /// reproduces the old behavior exactly.
    pub const DEFAULT_MAX_MESSAGES: usize = HISTORY_MAX_MESSAGES;

    /// Reads `FAMILYCLAW_COMPACTION_MAX_MESSAGES`,
    /// `FAMILYCLAW_COMPACTION_PROTECT_FIRST_N`, and
    /// `FAMILYCLAW_COMPACTION_PROTECT_LAST_N` (each optional, same
    /// "parse or default" shape as [`history_max_chars_per_msg`]). No env
    /// var carries a summarizer — install one via
    /// [`Agent::with_compaction`] instead. Called once by [`Agent::new`],
    /// so the threshold/protection knobs are configurable without a code
    /// change while staying fully backward-compatible when unset.
    #[must_use]
    pub fn from_env() -> Self {
        let max_messages = std::env::var("FAMILYCLAW_COMPACTION_MAX_MESSAGES")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(Self::DEFAULT_MAX_MESSAGES);
        let protect_first_n = std::env::var("FAMILYCLAW_COMPACTION_PROTECT_FIRST_N")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let protect_last_n = std::env::var("FAMILYCLAW_COMPACTION_PROTECT_LAST_N")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(Self::DEFAULT_MAX_MESSAGES);
        Self {
            max_messages,
            protect_first_n,
            protect_last_n,
            summarizer: None,
        }
    }
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            max_messages: Self::DEFAULT_MAX_MESSAGES,
            protect_first_n: 0,
            protect_last_n: Self::DEFAULT_MAX_MESSAGES,
            summarizer: None,
        }
    }
}

/// Applies `config` to `dq` in place: while the window exceeds
/// `config.max_messages`, evicts from the "middle" zone (between the
/// protected head [`CompactionConfig::protect_first_n`] and the protected
/// tail [`CompactionConfig::protect_last_n`]) — oldest evictable message
/// first, or (when a summarizer is installed and the middle zone has more
/// than one message) collapses the WHOLE middle zone into a single
/// synthetic message via the summarizer hook.
///
/// If the protected head + tail already cover the whole window, nothing
/// is evictable — the loop stops even if the window is still over
/// `max_messages` (protection wins over the threshold; this is a
/// deliberate trade-off, not a bug — a misconfigured
/// `protect_first_n + protect_last_n >= max_messages` just disables
/// compaction instead of evicting a "protected" message).
pub(crate) fn compact_history(dq: &mut VecDeque<LlmMessage>, config: &CompactionConfig) {
    if config.max_messages == 0 {
        return;
    }
    while dq.len() > config.max_messages {
        let len = dq.len();
        let middle_start = config.protect_first_n.min(len);
        let middle_end = len.saturating_sub(config.protect_last_n).max(middle_start);
        if middle_start >= middle_end {
            // Nothing evictable without violating a protected zone — stop.
            break;
        }
        // Only worth handing to the summarizer when it can actually shrink
        // the window (a 1-message "zone" summarized into 1 message would
        // spin forever); a single evictable message always takes the plain
        // path below.
        if middle_end - middle_start > 1 {
            if let Some(summarizer) = &config.summarizer {
                let middle: Vec<LlmMessage> = dq
                    .iter()
                    .skip(middle_start)
                    .take(middle_end - middle_start)
                    .cloned()
                    .collect();
                if let Some(summary) = summarizer(&middle) {
                    for _ in middle_start..middle_end {
                        dq.remove(middle_start);
                    }
                    dq.insert(middle_start, summary);
                    continue;
                }
                // Summarizer declined (`None`) → fall through to plain eviction.
            }
        }
        dq.remove(middle_start);
    }
}

/// Memory layer tag for a user's chat message (session-scoped hydration).
pub(super) const CHAT_USER_TAG: &str = "chat:user";

/// Memory layer tag for the agent's chat reply (session-scoped hydration).
pub(super) const CHAT_ASSISTANT_TAG: &str = "chat:assistant";

/// `Memory::source` for chat history entries (distinguishes them from bus memories).
pub(super) const CHAT_HISTORY_SOURCE: &str = "chat_history";

/// How many memories tagged with a chat role tag are fetched on cold start
/// (in-process RAM history empty → hydrate from store).
pub(super) const HISTORY_HYDRATE_LIMIT: usize = 20;

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
pub(crate) enum ToolLoopOutcome {
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
    /// **Context-compaction policy** applied to every conversation's
    /// [`history`](Self::history) window by
    /// [`compact_history`]/[`Agent::append_history`] (Hermes-style context
    /// management). Default [`CompactionConfig::default`] — reproduces the
    /// old fixed-size sliding window exactly (backward-compatible, no env
    /// read at construction so building an agent is deterministic
    /// regardless of the process environment). Callers that want the
    /// env-tunable knobs opt in explicitly with
    /// `with_compaction(CompactionConfig::from_env())`; a custom policy
    /// (protected zones + a summarizer hook) can be installed the same way
    /// via [`Agent::with_compaction`].
    compaction: CompactionConfig,
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
            compaction: CompactionConfig::default(),
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

    /// Installs a custom **context-compaction policy** ([`CompactionConfig`])
    /// for the per-conversation history window, replacing the
    /// [`CompactionConfig::from_env`] default set by [`Agent::new`]. Returns
    /// `self` for chaining. This is the seam for wiring protected
    /// zones (`protect_first_n`/`protect_last_n`) and a real summarizer
    /// model hook.
    #[must_use]
    pub fn with_compaction(mut self, config: CompactionConfig) -> Self {
        self.compaction = config;
        self
    }

    /// The agent's current context-compaction policy (read).
    #[must_use]
    pub fn compaction(&self) -> &CompactionConfig {
        &self.compaction
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
