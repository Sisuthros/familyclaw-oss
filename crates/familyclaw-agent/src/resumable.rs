//! Resumable turn state ([`ResumableTurn`]) + its crash-resistant store
//! ([`ResumableTurnStore`]) — the durable half of the suspend/resume
//! bridge (roadmap §6).
//!
//! ## Why this module exists
//! When [`Agent::think`](crate::Agent::think) runs the tool loop and a
//! tool requires human approval, the turn **suspends**
//! ([`ThinkOutcome::Suspended`](crate::ThinkOutcome::Suspended)).
//! Approval may arrive minutes or hours later — possibly only after a
//! process restart. To be able to **resume the turn from where it left
//! off**, the tool loop's state so far must be persisted: the message
//! stack (LLM context), the suspending tool call's identifier, and the
//! granted approval's identifier. This module persists exactly that —
//! nothing more.
//!
//! ## Secrecy invariant (absolute)
//! A resumable turn is **never** persisted with raw secrets or Layer B
//! data. [`ResumableTurn`] carries only a **SHA-256 hash**
//! ([`ResumableTurn::arguments_hash`]) and a **redacted summary**
//! ([`ResumableTurn::redacted_arguments`]) of the arguments — never raw
//! tool arguments. The message stack ([`ResumableTurn::messages`])
//! contains the tool loop's LLM context, which is already built from
//! **redacted evidence** (`familyclaw-actions` redacts evidence bundles
//! before their text is fed to the model) — it is the caller's
//! **responsibility** not to push secrets into the message stack, the
//! same as for
//! [`PendingRecord::redacted_summary`](familyclaw_actions::PendingRecord).
//!
//! Field by field, why no field leaks a secret — see [`ResumableTurn`]'s
//! documentation.
//!
//! ## Determinism
//! All time-reading logic takes the timestamp injected
//! ([`familyclaw_core::time::Timestamp`]) — the clock is never read
//! inside this module. Expiry uses the same fail-closed boundary as
//! [`familyclaw_actions::approval::Approval::is_expired`] (`now > expires_at`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use familyclaw_actions::approval::sha256_hex;
use familyclaw_actions::ApprovalId;
use familyclaw_bus::MessageOrigin;
use familyclaw_core::time::Timestamp;
use familyclaw_durable::{EntryKind, FileJournal, Journal, JournalEntry, StepId};

use crate::llm::LlmMessage;

/// This module's own error type (store I/O + serialization).
///
/// Kept separate from [`familyclaw_core::FamilyClawError`] so the store
/// stays thin and self-contained; [`crate::Agent`] wraps this into the
/// core type when needed.
#[derive(Debug)]
pub enum ResumableError {
    /// Opening, reading, or writing the journal failed.
    Journal(String),
    /// Serializing or parsing a [`ResumableTurn`] failed.
    Serde(String),
    /// The requested resumable turn was not found (unknown identifier, or
    /// already consumed/evicted). **Fail-closed:** an unknown identifier
    /// cannot be resumed.
    NotFound(ApprovalId),
    /// The resumable turn was found, but is **expired** (`now > expires_at`).
    /// Fail-closed: an expired turn is not resumed.
    Expired(ApprovalId),
}

impl std::fmt::Display for ResumableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResumableError::Journal(msg) => write!(f, "resumable journal error: {msg}"),
            ResumableError::Serde(msg) => write!(f, "resumable serde error: {msg}"),
            ResumableError::NotFound(id) => {
                write!(f, "resumable turn not found for approval {id}")
            }
            ResumableError::Expired(id) => {
                write!(f, "resumable turn expired for approval {id}")
            }
        }
    }
}

impl std::error::Error for ResumableError {}

/// This module's result type.
pub type Result<T> = std::result::Result<T, ResumableError>;

/// The **resumable turn**'s secret-free, persistent state (roadmap §6
/// resumable-turn-state).
///
/// This is exactly the information [`Agent::resume_approved`](crate::Agent::resume_approved)
/// needs to resume a suspended tool loop from where it left off — no
/// more. [`ResumableTurn::approval_id`] serves as the store's key.
///
/// ## Secrecy invariant (field by field)
/// No field carries a raw secret or Layer B data:
/// - [`approval_id`](Self::approval_id) — the granted approval's
///   identifier (UUID, not a secret). The store's key, and the link to
///   `familyclaw-actions`'s pending approval.
/// - [`being_id`](Self::being_id) — the being's bus identifier as a
///   string (UUID).
/// - [`conversation_origin`](Self::conversation_origin) — the reply
///   target (channel/conversation/sender). Routing metadata, not a secret.
/// - [`messages`](Self::messages) — the tool loop's LLM message stack.
///   Contains the system prompt, the user message, and the tool results
///   so far. Tool results are derived from **redacted evidence**
///   (`familyclaw-actions` redacts before the text is fed to the model),
///   so they contain no raw secrets. It's the caller's responsibility not
///   to push secrets in here.
/// - [`tool_call_id`](Self::tool_call_id) — the tool call identifier
///   given by the LLM (binds the upcoming `tool_result` message to the
///   right call). An opaque token.
/// - [`tool_name`](Self::tool_name) — the name of the suspending tool
///   (the manifest name, not a secret).
/// - [`arguments_hash`](Self::arguments_hash) — the tool arguments'
///   SHA-256 **hash** (not raw arguments). Binds the resumable turn
///   precisely to the arguments the approval was granted for.
/// - [`redacted_arguments`](Self::redacted_arguments) — a human-readable,
///   redacted summary of what the tool would do. **No raw arguments, no
///   secrets.**
/// - [`created_at`](Self::created_at) / [`expires_at`](Self::expires_at) —
///   timestamps (audit + TTL).
/// - [`policy_snapshot`](Self::policy_snapshot) — a policy snapshot at
///   the moment of suspension (e.g. the required permission). Neutral
///   metadata.
/// - [`audit_ids`](Self::audit_ids) — references to already-recorded
///   audit events (UUIDs), so resume can link itself to the suspension's
///   audit trail.
/// - [`turn_id`](Self::turn_id) / [`durable_cursor`](Self::durable_cursor) —
///   the turn's sequence number + the durable log's cursor position at
///   the moment of suspension. For diagnostics and resume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResumableTurn {
    /// The granted approval's identifier (the store's key).
    pub approval_id: ApprovalId,
    /// The bus identifier of the being that ran the turn, as a string.
    pub being_id: String,
    /// The reply target (channel/conversation/sender) for resuming;
    /// `None` if the turn had no per-message origin (a static target).
    pub conversation_origin: Option<MessageOrigin>,
    /// The tool loop's LLM message stack at the moment of suspension
    /// (system + user + the assistant/tool messages so far). The loop
    /// resumes from this.
    pub messages: Vec<LlmMessage>,
    /// The suspending tool call's LLM identifier (`tool_result` binds to
    /// this).
    pub tool_call_id: String,
    /// The name of the suspending tool (the manifest name).
    pub tool_name: String,
    /// The tool arguments' SHA-256 hash (NOT raw arguments).
    pub arguments_hash: String,
    /// A redacted, human-readable summary of the tool's arguments/action.
    pub redacted_arguments: String,
    /// The moment the suspension was created (audit).
    pub created_at: Timestamp,
    /// The moment after which the resumable turn is expired (= the
    /// approval's TTL).
    pub expires_at: Timestamp,
    /// A policy snapshot at the moment of suspension (neutral metadata).
    pub policy_snapshot: String,
    /// References to the suspension's audit events (UUID strings).
    pub audit_ids: Vec<String>,
    /// The turn's sequence number in the being's lifecycle at the moment
    /// of suspension.
    pub turn_id: u64,
    /// The durable log's cursor position at the moment of suspension
    /// (diagnostics).
    pub durable_cursor: u64,
}

impl ResumableTurn {
    /// Builds the resumable turn state **hashing the arguments**: raw
    /// arguments are not accepted; instead the caller already supplies
    /// the hash and the redacted summary. This makes it practically
    /// impossible to construct the type with a secret.
    ///
    /// `tool_arguments` is raw JSON from which **only** a SHA-256 hash is
    /// computed — the value itself is not stored. This is the payload
    /// binding's counterpart: when resume later continues, the approval
    /// is consumed against this same hash.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        approval_id: ApprovalId,
        being_id: impl Into<String>,
        conversation_origin: Option<MessageOrigin>,
        messages: Vec<LlmMessage>,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        tool_arguments: &serde_json::Value,
        redacted_arguments: impl Into<String>,
        created_at: Timestamp,
        expires_at: Timestamp,
    ) -> Self {
        // ONLY THE HASH OF THE ARGUMENTS IS STORED. Serialization practically
        // never fails (Value→Vec<u8>); if it does, the hash is computed
        // from empty bytes — that only blocks resume (mismatch), it leaks
        // nothing.
        let raw = serde_json::to_vec(tool_arguments).unwrap_or_default();
        let arguments_hash = sha256_hex(&raw);
        Self {
            approval_id,
            being_id: being_id.into(),
            conversation_origin,
            messages,
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            arguments_hash,
            redacted_arguments: redacted_arguments.into(),
            created_at,
            expires_at,
            policy_snapshot: String::new(),
            audit_ids: Vec::new(),
            turn_id: 0,
            durable_cursor: 0,
        }
    }

    /// Attaches the policy snapshot (chainable). Neutral metadata, not a
    /// secret.
    #[must_use]
    pub fn with_policy_snapshot(mut self, snapshot: impl Into<String>) -> Self {
        self.policy_snapshot = snapshot.into();
        self
    }

    /// Attaches the audit event identifiers (chainable).
    #[must_use]
    pub fn with_audit_ids(mut self, ids: Vec<String>) -> Self {
        self.audit_ids = ids;
        self
    }

    /// Attaches the durable position: turn number + cursor position
    /// (chainable).
    #[must_use]
    pub const fn with_durable_position(mut self, turn_id: u64, durable_cursor: u64) -> Self {
        self.turn_id = turn_id;
        self.durable_cursor = durable_cursor;
        self
    }

    /// Whether the resumable turn is expired relative to `now`
    /// (`now > expires_at`).
    ///
    /// Same fail-closed boundary as
    /// [`familyclaw_actions::approval::Approval::is_expired`]: exactly
    /// `expires_at` still counts as valid, genuinely later does not.
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now > self.expires_at
    }
}

/// **Store for resumable turns.**
///
/// Abstracts over where resumable turns live — in memory or on
/// crash-resistant disk. Same contract as
/// [`familyclaw_actions::PendingApprovalStore`]: all methods take
/// `&self` (internal mutation behind a lock), so the trait is
/// `dyn`-compatible.
///
/// ## Contract
/// - [`put`](Self::put) stores a resumable turn keyed by
///   `turn.approval_id`. Rewriting the same key replaces the previous one.
/// - [`get`](Self::get) returns the stored turn, `None` if not found.
/// - [`remove`](Self::remove) consumes (removes) the turn, one-time use.
/// - [`evict_expired`](Self::evict_expired) evicts expired turns using
///   the fail-closed boundary.
///
/// ## Secrets
/// An implementation that persists to disk may only write
/// [`ResumableTurn`]'s secret-free fields (hash + identifiers + redacted
/// summaries + the message stack derived from redacted evidence) — never
/// raw arguments or secrets.
pub trait ResumableTurnStore: Send + Sync {
    /// Stores (or replaces) a resumable turn keyed by `turn.approval_id`.
    ///
    /// # Errors
    /// [`ResumableError::Journal`]/[`ResumableError::Serde`] if the disk
    /// implementation's write or serialization fails.
    fn put(&self, turn: ResumableTurn) -> Result<()>;

    /// Looks up a resumable turn by approval identifier; `None` if not found.
    ///
    /// # Errors
    /// [`ResumableError::Journal`]/[`ResumableError::Serde`] for disk
    /// implementations.
    fn get(&self, approval_id: ApprovalId) -> Result<Option<ResumableTurn>>;

    /// Removes (consumes) a resumable turn and returns it, if it existed;
    /// `None` otherwise. One-time use: not found again after removal.
    ///
    /// # Errors
    /// [`ResumableError::Journal`]/[`ResumableError::Serde`] for disk
    /// implementations.
    fn remove(&self, approval_id: ApprovalId) -> Result<Option<ResumableTurn>>;

    /// The number of resumable turns.
    ///
    /// # Errors
    /// [`ResumableError::Journal`]/[`ResumableError::Serde`] for disk
    /// implementations.
    fn len(&self) -> Result<usize>;

    /// Whether the store is empty.
    ///
    /// # Errors
    /// Same as [`len`](Self::len).
    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Removes all turns expired as of `now` and returns the number
    /// evicted. Same fail-closed boundary as [`ResumableTurn::is_expired`].
    ///
    /// # Errors
    /// [`ResumableError::Journal`]/[`ResumableError::Serde`] for disk
    /// implementations.
    fn evict_expired(&self, now: Timestamp) -> Result<usize>;
}

/// An in-memory store ([`HashMap`] behind the trait).
///
/// Default and used in tests: fast, **but does not survive a crash**. In
/// production, where resume crash-resistance is a requirement, use
/// [`JournalResumableStore`].
#[derive(Debug, Default)]
pub struct InMemoryResumableStore {
    /// Approval identifier → resumable turn.
    inner: Mutex<HashMap<ApprovalId, ResumableTurn>>,
}

impl InMemoryResumableStore {
    /// Creates an empty in-memory store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the map, recovering from a poisoned lock without panicking.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ApprovalId, ResumableTurn>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ResumableTurnStore for InMemoryResumableStore {
    fn put(&self, turn: ResumableTurn) -> Result<()> {
        self.lock().insert(turn.approval_id, turn);
        Ok(())
    }

    fn get(&self, approval_id: ApprovalId) -> Result<Option<ResumableTurn>> {
        Ok(self.lock().get(&approval_id).cloned())
    }

    fn remove(&self, approval_id: ApprovalId) -> Result<Option<ResumableTurn>> {
        Ok(self.lock().remove(&approval_id))
    }

    fn len(&self) -> Result<usize> {
        Ok(self.lock().len())
    }

    fn evict_expired(&self, now: Timestamp) -> Result<usize> {
        let mut map = self.lock();
        let before = map.len();
        map.retain(|_, turn| !turn.is_expired(now));
        Ok(before - map.len())
    }
}

/// The journal line's logical name for storing a resumable turn.
const RESUMABLE_PUT: &str = "resumable_turn_put";
/// The journal line's logical name for removing a resumable turn (tombstone).
const RESUMABLE_DELETE: &str = "resumable_turn_delete";

/// Default compaction factor: the log is compacted automatically once
/// the number of physical rows exceeds
/// `AUTO_COMPACT_FACTOR * number_of_live_turns`.
///
/// A factor of 2 = "compact once at least half the rows are dead". Bounds
/// the accumulation of dead rows to a constant multiple of live rows, so
/// the log size and replay's O(n) cost stay proportional to the size of
/// the live state.
const AUTO_COMPACT_FACTOR: usize = 2;

/// Minimum number of physical rows for auto-compaction to even be
/// considered (avoids pointless compaction on small logs).
const AUTO_COMPACT_MIN_ROWS: usize = 64;

/// A crash-resistant store built on [`FileJournal`].
///
/// An append-only log: every store is written as a `resumable_turn_put`
/// marker (the whole secret-free [`ResumableTurn`]) and every removal as
/// a `resumable_turn_delete` marker (just the approval identifier, a
/// tombstone). State is reconstructed by replaying the log: a later row
/// wins, so a removal cancels out an addition.
///
/// Because [`FileJournal::append`] flushes and fsyncs before returning,
/// a completed store is on disk even after a sudden crash — **a
/// resumable turn survives a crash between suspension and resume**, so
/// after approval is granted, the turn can be resumed to completion even
/// if the process restarted in between.
///
/// ## Compaction — keeping unbounded growth in check
/// Because the log is append-only, every removal
/// ([`remove`](ResumableTurnStore::remove) /
/// [`evict_expired`](ResumableTurnStore::evict_expired)) and every
/// replacement of the same identifier leaves **dead rows** in the log:
/// the state is correct (the later row wins), but the file grows
/// unbounded and replay becomes O(n) in row count.
/// [`compact`](JournalResumableStore::compact) rewrites the log to
/// contain **only live turns**, atomically, via [`FileJournal::rewrite`]
/// (temp + fsync + rename) — the live state is preserved bit-for-bit and
/// an interruption never loses live turns. Compaction is triggered either
/// by the operator or **automatically** alongside store/eviction once
/// the dead-row ratio exceeds the threshold (see `AUTO_COMPACT_FACTOR`
/// and [`with_auto_compact_factor`](JournalResumableStore::with_auto_compact_factor)).
///
/// ## Secrecy invariant
/// Only [`ResumableTurn`]'s secret-free fields are written to disk (see
/// [`ResumableTurn`]) — never raw tool arguments or secrets. Compaction
/// preserves this: the rewritten log contains the same secret-free
/// `resumable_turn_put` rows.
pub struct JournalResumableStore {
    /// The append-only log that stores and removals are recorded to.
    journal: FileJournal,
    /// The next row's sequence position (monotonic).
    next_step: Mutex<StepId>,
    /// The auto-compaction factor: compact when `rows > factor * live`.
    /// `0` disables auto-compaction (manual `compact` only).
    auto_compact_factor: usize,
}

impl std::fmt::Debug for JournalResumableStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalResumableStore")
            .field("path", &self.journal.path())
            .finish_non_exhaustive()
    }
}

impl JournalResumableStore {
    /// Opens (or creates) a crash-resistant store from the given file path.
    ///
    /// Resumable turns are reconstructed immediately from an existing
    /// log, so after a restart they are still retrievable via
    /// [`get`](ResumableTurnStore::get) and resumable.
    ///
    /// # Errors
    /// [`ResumableError::Journal`] if the journal cannot be opened or read.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let journal = FileJournal::open(path)
            .map_err(|e| ResumableError::Journal(format!("open resumable journal failed: {e}")))?;
        let len = journal
            .len()
            .map_err(|e| ResumableError::Journal(format!("read resumable journal failed: {e}")))?;
        let next = StepId::new(u64::try_from(len).unwrap_or(u64::MAX));
        Ok(Self {
            journal,
            next_step: Mutex::new(next),
            auto_compact_factor: AUTO_COMPACT_FACTOR,
        })
    }

    /// Sets the auto-compaction factor (chainable).
    ///
    /// The log is compacted automatically once the number of physical
    /// rows exceeds `factor * number_of_live_turns` (and there are at
    /// least `AUTO_COMPACT_MIN_ROWS` rows). The default is
    /// `AUTO_COMPACT_FACTOR` (2). A value of `0` **disables**
    /// auto-compaction — the log is compacted only via an explicit
    /// [`compact`](Self::compact) call.
    #[must_use]
    pub const fn with_auto_compact_factor(mut self, factor: usize) -> Self {
        self.auto_compact_factor = factor;
        self
    }

    /// Returns the log's file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.journal.path()
    }

    /// Reserves and returns the next sequence position (monotonic).
    fn next_step_id(&self) -> StepId {
        let mut guard = self
            .next_step
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = *guard;
        *guard = current.next();
        current
    }

    /// Appends a marker to the log with the given name and payload.
    fn append_marker(&self, name: &str, payload: serde_json::Value) -> Result<()> {
        let entry = JournalEntry::marker(self.next_step_id(), name, payload);
        self.journal
            .append(entry)
            .map_err(|e| ResumableError::Journal(format!("append resumable marker failed: {e}")))
    }

    /// Reconstructs the current state by replaying the log (a later row
    /// wins).
    fn replay_state(&self) -> Result<HashMap<ApprovalId, ResumableTurn>> {
        let entries = self.journal.replay_all().map_err(|e| {
            ResumableError::Journal(format!("replay resumable journal failed: {e}"))
        })?;
        Self::reconstruct_state(entries)
    }

    /// Builds the current state from the given journal rows (a pure
    /// function, no I/O).
    ///
    /// The replay walks rows in order: `resumable_turn_put` adds/replaces
    /// a turn, `resumable_turn_delete` removes it (tombstone). A later
    /// row wins. Kept separate from [`replay_state`](Self::replay_state)
    /// so that both the disk-reading replay and
    /// [`compact`](Self::compact)'s [`FileJournal::compact_with`] closure
    /// build state using **the same logic** — the latter receives the
    /// rows already read under the lock, and must not read the journal
    /// again (deadlock).
    fn reconstruct_state(entries: Vec<JournalEntry>) -> Result<HashMap<ApprovalId, ResumableTurn>> {
        let mut state: HashMap<ApprovalId, ResumableTurn> = HashMap::new();
        for entry in entries {
            let EntryKind::Marker { name, payload } = entry.kind else {
                continue;
            };
            match name.as_str() {
                RESUMABLE_PUT => {
                    let turn: ResumableTurn = serde_json::from_value(payload).map_err(|e| {
                        ResumableError::Serde(format!("decode resumable put failed: {e}"))
                    })?;
                    state.insert(turn.approval_id, turn);
                }
                RESUMABLE_DELETE => {
                    let id: ApprovalId = serde_json::from_value(payload).map_err(|e| {
                        ResumableError::Serde(format!("decode resumable delete id failed: {e}"))
                    })?;
                    state.remove(&id);
                }
                _ => {}
            }
        }
        Ok(state)
    }

    /// The number of physical journal rows (live + dead). The
    /// measurement basis for the dead-row ratio; differs from
    /// [`len`](ResumableTurnStore::len), which returns only the number of
    /// live turns.
    fn physical_row_count(&self) -> Result<usize> {
        self.journal
            .len()
            .map_err(|e| ResumableError::Journal(format!("read resumable journal len failed: {e}")))
    }

    /// Rewrites the log to contain **only live turns** (compaction),
    /// dropping all dead rows (tombstones and superseded `put` rows).
    /// Returns the number of dead rows dropped.
    ///
    /// The live state is preserved bit-for-bit: after compaction exactly
    /// the same turns are retrievable via [`get`](ResumableTurnStore::get),
    /// and reloading (restart) reconstructs an identical state.
    /// Compaction is **atomic** ([`FileJournal::rewrite`]: temp + fsync +
    /// rename) — if the process crashes mid-way, the live file is still
    /// in its old, intact state and no live turn is lost.
    ///
    /// Rows are renumbered into a compact `0..N` sequence and the
    /// internal sequence cursor is set to match, so future stores
    /// continue from the correct position.
    ///
    /// # Errors
    /// [`ResumableError::Serde`] if serializing some turn fails;
    /// [`ResumableError::Journal`] if reading the log or the atomic
    /// rewrite fails. On error, the live log is left unchanged.
    pub fn compact(&self) -> Result<usize> {
        // Atomic compaction against appends: [`FileJournal::compact_with`]
        // holds the same file lock for the whole
        // read→filter→swap operation, so a concurrent store/removal
        // cannot land in the gap and get lost (a TOCTOU fix). The
        // `build` closure receives the rows already read, reconstructs
        // the live state, and returns the renumbered live
        // RESUMABLE_PUT rows.
        //
        // The number of live rows is smuggled out of the closure via a
        // `Cell`, so the sequence cursor can be set after the swap (the
        // closure must NOT lock the journal again → it cannot read the
        // cursor via its own path).
        let live_count = std::cell::Cell::new(0usize);
        let dropped = self
            .journal
            .compact_with(|entries| {
                // Reconstruct the live state from the already-read rows
                // (same logic as replay, but WITHOUT re-reading — that
                // would lock the journal again and deadlock).
                // ResumableError is wrapped as DurableError text so the
                // type fits the compact_with contract.
                let state = Self::reconstruct_state(entries).map_err(|e| {
                    familyclaw_durable::DurableError::step_failed(
                        "compact_reconstruct",
                        e.to_string(),
                    )
                })?;
                // One RESUMABLE_PUT row per live turn, renumbered 0..N.
                let mut kept = Vec::with_capacity(state.len());
                let mut step = StepId::ZERO;
                for turn in state.values() {
                    let payload = serde_json::to_value(turn)?;
                    kept.push(JournalEntry::marker(step, RESUMABLE_PUT, payload));
                    step = step.next();
                }
                live_count.set(kept.len());
                Ok(kept)
            })
            .map_err(|e| {
                ResumableError::Journal(format!("compact resumable journal failed: {e}"))
            })?;

        // Point the sequence cursor past the compacted log (= the number
        // of live turns, since rows were renumbered compactly as 0..N).
        {
            let mut guard = self
                .next_step
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = StepId::new(u64::try_from(live_count.get()).unwrap_or(u64::MAX));
        }

        Ok(dropped)
    }

    /// Compacts the log **if** the dead-row ratio exceeds the threshold.
    ///
    /// Trigger condition: `auto_compact_factor > 0` AND there are at
    /// least [`AUTO_COMPACT_MIN_ROWS`] physical rows AND
    /// `rows > factor * live`. Called after store and eviction.
    /// Auto-compaction failing does **not** fail the caller: the data is
    /// already safely in the log, so compaction is purely an
    /// optimization — the error is swallowed (the log just stays
    /// uncompacted this time).
    fn maybe_auto_compact(&self) {
        if self.auto_compact_factor == 0 {
            return;
        }
        let Ok(rows) = self.physical_row_count() else {
            return;
        };
        if rows < AUTO_COMPACT_MIN_ROWS {
            return;
        }
        let Ok(live) = self.replay_state().map(|s| s.len()) else {
            return;
        };
        if rows > self.auto_compact_factor.saturating_mul(live) {
            let _ = self.compact();
        }
    }
}

impl ResumableTurnStore for JournalResumableStore {
    fn put(&self, turn: ResumableTurn) -> Result<()> {
        let payload = serde_json::to_value(&turn)
            .map_err(|e| ResumableError::Serde(format!("encode resumable turn failed: {e}")))?;
        self.append_marker(RESUMABLE_PUT, payload)?;
        // A replacement left a dead row (the old put) → consider auto-compaction.
        self.maybe_auto_compact();
        Ok(())
    }

    fn get(&self, approval_id: ApprovalId) -> Result<Option<ResumableTurn>> {
        Ok(self.replay_state()?.remove(&approval_id))
    }

    fn remove(&self, approval_id: ApprovalId) -> Result<Option<ResumableTurn>> {
        let existing = self.replay_state()?.remove(&approval_id);
        if existing.is_some() {
            // Only tombstone if the turn existed — avoids a needless row.
            let payload = serde_json::to_value(approval_id).map_err(|e| {
                ResumableError::Serde(format!("encode resumable delete id failed: {e}"))
            })?;
            self.append_marker(RESUMABLE_DELETE, payload)?;
            // A tombstone is a dead row → consider auto-compaction.
            self.maybe_auto_compact();
        }
        Ok(existing)
    }

    fn len(&self) -> Result<usize> {
        Ok(self.replay_state()?.len())
    }

    fn evict_expired(&self, now: Timestamp) -> Result<usize> {
        let state = self.replay_state()?;
        let expired: Vec<ApprovalId> = state
            .values()
            .filter(|turn| turn.is_expired(now))
            .map(|turn| turn.approval_id)
            .collect();
        for id in &expired {
            let payload = serde_json::to_value(id).map_err(|e| {
                ResumableError::Serde(format!("encode resumable delete id failed: {e}"))
            })?;
            self.append_marker(RESUMABLE_DELETE, payload)?;
        }
        if !expired.is_empty() {
            // Eviction produced tombstones (dead rows) → consider compaction.
            self.maybe_auto_compact();
        }
        Ok(expired.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use familyclaw_core::time::from_unix_secs;
    use std::path::PathBuf;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    /// Helper: a resumable turn with the given TTL and arguments.
    fn turn_at(now: Timestamp, ttl: Duration, args: &serde_json::Value) -> ResumableTurn {
        ResumableTurn::new(
            ApprovalId::new(),
            BeingIdStr(),
            Some(MessageOrigin::new("discord-main", "conv-1", "user-9")),
            vec![
                LlmMessage::system("you are a generic being"),
                LlmMessage::user("draft a github issue"),
            ],
            "call_abc123",
            "github_issue_draft",
            args,
            "github_issue_draft({title: <redacted>})",
            now,
            now + ttl,
        )
    }

    /// Generic being-id string for tests (not a secret).
    #[allow(non_snake_case)]
    fn BeingIdStr() -> String {
        "00000000-0000-4000-8000-000000000001".to_string()
    }

    /// An RAII temp file without external crates.
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "familyclaw-resumable-{tag}-{}-{:?}.jsonl",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            let _ = std::fs::remove_file(&p);
            Self(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn in_memory_put_get_remove_roundtrip() {
        let store = InMemoryResumableStore::new();
        let now = at(1_700_000_000);
        let args = serde_json::json!({ "title": "Login broken" });
        let turn = turn_at(now, Duration::minutes(60), &args);
        let id = turn.approval_id;

        store.put(turn).expect("put");
        assert_eq!(store.len().expect("len"), 1);

        let got = store.get(id).expect("get").expect("present");
        assert_eq!(got.approval_id, id);
        assert_eq!(got.tool_name, "github_issue_draft");
        // Payload binding: the hash matches the same argument value.
        assert_eq!(
            got.arguments_hash,
            sha256_hex(&serde_json::to_vec(&args).unwrap())
        );

        let removed = store.remove(id).expect("remove").expect("present");
        assert_eq!(removed.approval_id, id);
        assert!(store.get(id).expect("get").is_none());
        assert!(store.is_empty().expect("empty"));
    }

    #[test]
    fn arguments_are_hashed_not_stored_raw() {
        // The argument contains a secret — only the hash is stored.
        let now = at(1_700_000_000);
        let secret = format!("sk-{}", "live".repeat(4));
        let args = serde_json::json!({ "api_key": secret });
        let turn = turn_at(now, Duration::minutes(60), &args);
        // The serialized form contains no raw secret.
        let json = serde_json::to_string(&turn).expect("serialize");
        assert!(
            !json.contains(&secret),
            "raw secret must never be in the turn"
        );
        // But the hash is present.
        assert!(json.contains(&sha256_hex(&serde_json::to_vec(&args).unwrap())));
    }

    #[test]
    fn ttl_eviction_drops_expired_only() {
        let store = InMemoryResumableStore::new();
        let now = at(1_700_000_000);
        let args = serde_json::json!({ "x": 1 });
        let short = turn_at(now, Duration::seconds(60), &args);
        let long = turn_at(now, Duration::seconds(3600), &args);
        let short_id = short.approval_id;
        let long_id = long.approval_id;
        store.put(short).expect("put short");
        store.put(long).expect("put long");

        let evicted = store.evict_expired(at(1_700_000_120)).expect("evict");
        assert_eq!(evicted, 1);
        assert!(store.get(short_id).expect("get").is_none());
        assert!(store.get(long_id).expect("get").is_some());
    }

    #[test]
    fn ttl_boundary_keeps_exactly_at_expiry() {
        let store = InMemoryResumableStore::new();
        let now = at(1_700_000_000);
        let args = serde_json::json!({ "x": 1 });
        let turn = turn_at(now, Duration::seconds(60), &args);
        let id = turn.approval_id;
        store.put(turn).expect("put");

        // Exactly at expires_at is NOT expired yet (same fail-closed boundary).
        assert_eq!(store.evict_expired(at(1_700_000_060)).expect("evict"), 0);
        assert!(store.get(id).expect("get").is_some());
        assert_eq!(store.evict_expired(at(1_700_000_061)).expect("evict"), 1);
        assert!(store.get(id).expect("get").is_none());
    }

    #[test]
    fn durable_reloads_after_simulated_restart() {
        let tmp = TempPath::new("reload");
        let now = at(1_700_000_000);
        let args = serde_json::json!({ "title": "Bug" });
        let turn = turn_at(now, Duration::minutes(60), &args);
        let id = turn.approval_id;
        let hash = turn.arguments_hash.clone();

        // Step 1: store and DROP (simulates a crash).
        {
            let store = JournalResumableStore::open(tmp.path()).expect("open 1");
            store.put(turn).expect("put");
            assert_eq!(store.len().expect("len"), 1);
        }

        // Step 2: rebuild the store AGAIN from the same file — the turn survived.
        let resumed = JournalResumableStore::open(tmp.path()).expect("open 2");
        assert_eq!(resumed.len().expect("len"), 1, "resumable survived restart");
        let got = resumed.get(id).expect("get").expect("still present");
        assert_eq!(got.approval_id, id);
        assert_eq!(got.arguments_hash, hash);
        assert_eq!(got.messages.len(), 2, "message stack survived");

        // Consumption survives as a tombstone across yet another restart.
        resumed.remove(id).expect("remove").expect("present");
        let after = JournalResumableStore::open(tmp.path()).expect("open 3");
        assert!(after.get(id).expect("get").is_none());
        assert!(after.is_empty().expect("empty"));
    }

    #[test]
    fn durable_persisted_form_contains_no_raw_secret() {
        let tmp = TempPath::new("no-secret");
        let now = at(1_700_000_000);
        let secret = format!("sk-{}", "live".repeat(4));
        let args = serde_json::json!({ "api_key": secret.clone() });
        let turn = turn_at(now, Duration::minutes(60), &args);

        let store = JournalResumableStore::open(tmp.path()).expect("open");
        store.put(turn).expect("put");

        let on_disk = std::fs::read_to_string(tmp.path()).expect("read journal");
        assert!(
            !on_disk.contains(&secret),
            "persisted resumable turn must never contain the raw secret"
        );
        // The hash IS present (the binding is preserved).
        assert!(on_disk.contains(&sha256_hex(&serde_json::to_vec(&args).unwrap())));
    }

    #[test]
    fn get_unknown_is_none() {
        let store = InMemoryResumableStore::new();
        assert!(store.get(ApprovalId::new()).expect("get").is_none());
    }

    // ---- Compaction ----

    /// Counts physical (live + dead) journal rows by reading the file.
    fn physical_rows(path: &Path) -> usize {
        std::fs::read_to_string(path)
            .map_or(0, |s| s.lines().filter(|l| !l.trim().is_empty()).count())
    }

    /// Helper: store `n` turns, return their identifiers.
    fn put_n(store: &JournalResumableStore, now: Timestamp, n: usize) -> Vec<ApprovalId> {
        let args = serde_json::json!({ "x": 1 });
        let mut ids = Vec::with_capacity(n);
        for _ in 0..n {
            let turn = turn_at(now, Duration::minutes(60), &args);
            ids.push(turn.approval_id);
            store.put(turn).expect("put");
        }
        ids
    }

    #[test]
    fn compact_drops_dead_rows_keeps_live_turns() {
        let tmp = TempPath::new("compact-basic");
        let now = at(1_700_000_000);
        let store = JournalResumableStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        let ids = put_n(&store, now, 10);
        for id in ids.iter().take(5) {
            store.remove(*id).expect("remove");
        }
        assert_eq!(physical_rows(tmp.path()), 15, "10 put + 5 tombstone");
        assert_eq!(store.len().expect("len"), 5);

        let dropped = store.compact().expect("compact");
        assert_eq!(dropped, 10, "15 rows → 5 live = 10 dropped");
        assert_eq!(physical_rows(tmp.path()), 5);
        assert_eq!(store.len().expect("len"), 5);

        for id in ids.iter().take(5) {
            assert!(store.get(*id).expect("get").is_none(), "removed gone");
        }
        for id in ids.iter().skip(5) {
            assert!(store.get(*id).expect("get").is_some(), "live present");
        }
    }

    #[test]
    fn compact_preserves_exact_state_across_reload() {
        let tmp = TempPath::new("compact-reload");
        let now = at(1_700_000_000);

        let ids = {
            let store = JournalResumableStore::open(tmp.path())
                .expect("open 1")
                .with_auto_compact_factor(0);
            let ids = put_n(&store, now, 6);
            for id in ids.iter().take(3) {
                store.remove(*id).expect("remove");
            }
            store.compact().expect("compact");
            ids
        };

        // Restart from just the compacted file → identical state.
        let resumed = JournalResumableStore::open(tmp.path()).expect("open 2");
        assert_eq!(resumed.len().expect("len"), 3);
        for id in ids.iter().take(3) {
            assert!(
                resumed.get(*id).expect("get").is_none(),
                "removed stay removed"
            );
        }
        for id in ids.iter().skip(3) {
            assert!(resumed.get(*id).expect("get").is_some(), "live stay live");
        }
        assert_eq!(physical_rows(tmp.path()), 3);
    }

    #[test]
    fn compact_is_atomic_temp_then_rename() {
        let tmp = TempPath::new("compact-atomic");
        let now = at(1_700_000_000);
        let store = JournalResumableStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        let ids = put_n(&store, now, 8);
        for id in &ids {
            store.remove(*id).expect("remove");
        }
        let live = put_n(&store, now, 1);
        store.compact().expect("compact");

        // Every row on disk parses intact (no half-written row from the rename).
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read");
        for line in on_disk.lines().filter(|l| !l.trim().is_empty()) {
            serde_json::from_str::<JournalEntry>(line).expect("intact json line");
        }
        assert_eq!(store.len().expect("len"), 1);
        assert!(store.get(live[0]).expect("get").is_some());

        // No orphaned temp file under this log's name.
        let dir = tmp.path().parent().expect("parent");
        let own = tmp
            .path()
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let leftover: Vec<_> = std::fs::read_dir(dir)
            .expect("read_dir")
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&own) && n.contains(".compact-") && n.contains(".tmp"))
            .collect();
        assert!(leftover.is_empty(), "no temp left: {leftover:?}");
    }

    #[test]
    fn compact_drops_expired_turns() {
        let tmp = TempPath::new("compact-expired");
        let now = at(1_700_000_000);
        let store = JournalResumableStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        let args = serde_json::json!({ "x": 1 });
        let short = turn_at(now, Duration::seconds(60), &args);
        let short_id = short.approval_id;
        let long = turn_at(now, Duration::seconds(3600), &args);
        let long_id = long.approval_id;
        store.put(short).expect("put short");
        store.put(long).expect("put long");

        assert_eq!(store.evict_expired(at(1_700_000_120)).expect("evict"), 1);
        store.compact().expect("compact");

        assert!(store.get(short_id).expect("get").is_none());
        assert!(store.get(long_id).expect("get").is_some());
        assert_eq!(physical_rows(tmp.path()), 1, "only the live turn remains");
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read");
        assert!(
            !on_disk.contains(&short_id.to_string()),
            "expired id gone from disk"
        );
    }

    #[test]
    fn auto_compact_triggers_when_dead_rows_exceed_threshold() {
        let tmp = TempPath::new("auto-compact");
        let now = at(1_700_000_000);
        let store = JournalResumableStore::open(tmp.path()).expect("open"); // default factor
        let args = serde_json::json!({ "x": 1 });

        // One permanent live turn.
        let keeper = turn_at(now, Duration::minutes(60), &args);
        let keeper_id = keeper.approval_id;
        store.put(keeper).expect("put keeper");

        // 100 put+remove pairs → 201 rows without compaction.
        for _ in 0..100 {
            let t = turn_at(now, Duration::minutes(60), &args);
            let id = t.approval_id;
            store.put(t).expect("put churn");
            store.remove(id).expect("remove churn");
        }

        let rows = physical_rows(tmp.path());
        assert!(
            rows < 50,
            "auto-compaction should keep log small, got {rows}"
        );
        assert!(store.get(keeper_id).expect("get").is_some());
        assert_eq!(store.len().expect("len"), 1);
    }

    #[test]
    fn compact_on_empty_log_is_noop() {
        let tmp = TempPath::new("compact-empty");
        let store = JournalResumableStore::open(tmp.path()).expect("open");
        assert_eq!(store.compact().expect("compact"), 0);
        assert!(store.is_empty().expect("empty"));
        assert_eq!(physical_rows(tmp.path()), 0);
    }

    /// Regression test for closing the TOCTOU gap: compaction reads the
    /// state and rewrites the log **under the same file lock**
    /// ([`FileJournal::compact_with`]), so a concurrent store cannot land
    /// in the gap and get lost. This does not run genuine concurrency
    /// (the race is non-deterministic) — instead it proves the
    /// observable invariant that follows from the structure: a store
    /// made AFTER compaction returns lands AFTER the compacted live
    /// turns, and reloading produces exactly the correct state.
    /// Concurrent-append safety now follows from holding a single lock.
    #[test]
    fn compact_then_put_does_not_lose_post_compact_turn() {
        let tmp = TempPath::new("compact-toctou");
        let now = at(1_700_000_000);
        let store = JournalResumableStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        let ids = put_n(&store, now, 6);
        for id in ids.iter().take(3) {
            store.remove(*id).expect("remove");
        }
        assert_eq!(store.len().expect("len"), 3, "3 live before compact");

        let dropped = store.compact().expect("compact");
        assert_eq!(
            dropped, 6,
            "9 rows (6 put + 3 tombstone) → 3 live = 6 dropped"
        );
        assert_eq!(physical_rows(tmp.path()), 3, "only live rows after compact");

        // A turn stored AFTER compaction lands AFTER the live turns.
        let args = serde_json::json!({ "x": 2 });
        let post = turn_at(now, Duration::minutes(60), &args);
        let post_id = post.approval_id;
        store.put(post).expect("put after compact");
        assert_eq!(store.len().expect("len"), 4, "3 live + 1 post-compact");
        assert!(store.get(post_id).expect("get").is_some());

        // Reloading produces exactly the correct state.
        let resumed = JournalResumableStore::open(tmp.path()).expect("reopen");
        assert_eq!(resumed.len().expect("len"), 4);
        for id in ids.iter().take(3) {
            assert!(
                resumed.get(*id).expect("get").is_none(),
                "removed stay removed"
            );
        }
        for id in ids.iter().skip(3) {
            assert!(resumed.get(*id).expect("get").is_some(), "live stay live");
        }
        assert!(
            resumed.get(post_id).expect("get").is_some(),
            "post-compact survives reload"
        );
    }
}
