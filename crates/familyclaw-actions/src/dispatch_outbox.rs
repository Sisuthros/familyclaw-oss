//! Dispatch idempotency outbox ([`DispatchOutboxStore`]) — Layer A.
//!
//! ## The problem this solves (the cornerstone of the exactly-once boundary)
//! [`crate::facade::ActionRuntime::submit_task`] performs an **external
//! side effect** (running the pipeline, storing the proof, recording a
//! pending approval). The agent layer wraps this in a durable step, but the
//! side effect's **execution** and the step's **journaling** are two separate
//! events: there is a WINDOW between them. If the process is killed
//! (SIGKILL) right in that window — the side effect has already happened but
//! the journal row does not exist — replay does not see the row, assumes the
//! step never ran, and **runs `submit_task` again**. Result: a double-fire,
//! which violates the "exactly-once side effects under SIGKILL" invariant.
//!
//! Simply moving the journaling to **before** the side effect does not fix
//! this either: then a side effect that never actually happened (crashed
//! before execution) could be journaled → a false "exactly-once" in the
//! other direction.
//!
//! ## Solution: an idempotency key at the runtime BOUNDARY
//! The outbox attaches a **stable idempotency key** to every dispatch (the
//! caller derives it deterministically, e.g. `turn-{turn}-dispatch-{k}`).
//! The dispatch is recorded in two phases to a **crash-durable** log:
//!
//! 1. **intent** (`DISPATCH_INTENT`) is recorded **BEFORE** the side effect.
//! 2. the side effect is executed.
//! 3. **committed** (`DISPATCH_COMMITTED`) is recorded after the side
//!    effect, containing the dispatch outcome ([`DispatchedOutcome`]).
//!
//! When the same key is seen again (replay or restart):
//! - **committed** is found → the stored outcome is returned
//!   **value-identically** (same `task_id` / `ApprovalId` / TTL) **without
//!   re-running the side effect**.
//! - **intent but no committed** → the process crashed mid-side-effect. The
//!   side effect may have partially happened → the recovery policy is
//!   **explicit and fail-closed** ([`DispatchLookup::InProgress`]): the call
//!   is NOT re-run — it is rejected instead, so it is never blindly
//!   duplicated.
//! - **neither** → the key was never started → it is safe to perform the
//!   side effect.
//!
//! ## The exact boundary of the guarantee (stated honestly)
//! - **Process crash / SIGKILL:** guaranteed. The side effect runs at most
//!   once; a dispatch that reached the committed state is returned
//!   identically and is never re-run.
//! - **Power loss / directory metadata fsync:** like
//!   [`crate::pending_store::JournalPendingStore`], this relies on
//!   [`familyclaw_durable::FileJournal`]'s `flush` + `fsync` guarantee for
//!   the *file's* contents. For the directory entry (dir-fsync) and
//!   hardware write buffers, the guarantee is only as strong as the
//!   underlying FS/hardware — this is **not overclaimed**. An intent-only
//!   trace after a crash is always detectable, and the recovery policy for
//!   that case is explicit.
//!
//! ## Secrecy invariant
//! The stored form ([`DispatchedOutcome`]) contains only identifiers, the
//! status, and a possible approval identifier + error message — **no raw
//! payload and no secrets**. Same invariant as
//! [`crate::pending_store::PendingRecord`].
//!
//! ## Determinism
//! The clock is never read inside this module; the idempotency key is
//! supplied by the caller.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use familyclaw_durable::{EntryKind, FileJournal, Journal, JournalEntry, StepId};

use crate::error::{ActionError, Result};
use crate::facade::SubmitOutcome;
use crate::ids::{ActionTaskId, ApprovalId};
use crate::task::TaskStatus;

/// Module readiness level — kept so that [`crate::all_modules_scaffolded`]
/// still compiles alongside other modules that are still in the scaffold
/// stage.
pub(crate) const SCAFFOLDED: bool = true;

/// Journal row logical name for a dispatch's **intent** (recorded BEFORE the side effect).
const DISPATCH_INTENT: &str = "dispatch_intent";
/// Journal row logical name for a dispatch's **commitment** (recorded after the side effect).
const DISPATCH_COMMITTED: &str = "dispatch_committed";

/// A dispatch's journalable, **secret-free** outcome.
///
/// This is the stored form of [`SubmitOutcome`] in the outbox: exactly the
/// part the caller needs to continue — `task_id`, `status`, and a possible
/// `pending_approval` — plus a possible `submit_task` error message, so that
/// even a failed dispatch is returned identically without re-running the
/// side effect.
///
/// Contains no raw payload and no secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchedOutcome {
    /// The identifier of the dispatched task.
    pub task_id: ActionTaskId,
    /// The task's status after the dispatch.
    pub status: TaskStatus,
    /// The approval identifier if the dispatch was left pending approval (otherwise `None`).
    pub pending_approval: Option<ApprovalId>,
    /// `submit_task`'s error message if the dispatch failed (otherwise `None`).
    pub error: Option<String>,
}

impl DispatchedOutcome {
    /// Builds the journalable outcome for a successful dispatch.
    #[must_use]
    pub const fn from_submit(outcome: &SubmitOutcome) -> Self {
        Self {
            task_id: outcome.task_id,
            status: outcome.status,
            pending_approval: outcome.pending_approval,
            error: None,
        }
    }

    /// Builds the journalable outcome for a failed dispatch.
    ///
    /// Stores the error message (nil identifier + [`TaskStatus::Failed`]) so
    /// that replay returns the same error without re-running the side effect.
    #[must_use]
    pub fn from_error(message: impl Into<String>) -> Self {
        Self {
            task_id: ActionTaskId::nil(),
            status: TaskStatus::Failed,
            pending_approval: None,
            error: Some(message.into()),
        }
    }

    /// Returns this outcome as a [`Result<SubmitOutcome>`].
    ///
    /// If the stored outcome carried an error, returns
    /// [`ActionError::ExecutionFailed`] with the same message; otherwise a
    /// successful [`SubmitOutcome`] value-identical to the original dispatch.
    ///
    /// # Errors
    /// [`ActionError::ExecutionFailed`] if the stored dispatch was an error.
    pub fn into_result(self) -> Result<SubmitOutcome> {
        if let Some(message) = self.error {
            return Err(ActionError::ExecutionFailed(message));
        }
        Ok(SubmitOutcome {
            task_id: self.task_id,
            status: self.status,
            pending_approval: self.pending_approval,
        })
    }
}

/// The state of a single idempotency key in the outbox.
///
/// Returned by [`DispatchOutboxStore::lookup`] so the caller knows whether
/// it is safe to perform the side effect, whether it already ran, or
/// whether it crashed mid-flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchLookup {
    /// The key was never started → it is safe to perform the side effect.
    NotStarted,
    /// The dispatch has already committed → the outcome must be returned without re-running.
    Committed(DispatchedOutcome),
    /// Intent was recorded but not commitment → the process crashed mid-side-effect.
    /// Recovery policy: fail-closed (do not re-run).
    InProgress,
}

/// The internal reconstructed state of a single key (intent seen? committed seen?).
#[derive(Debug, Clone, Default)]
struct KeyState {
    /// Whether intent (`DISPATCH_INTENT`) has been recorded for this key.
    intent: bool,
    /// The committed outcome, if `DISPATCH_COMMITTED` was recorded.
    committed: Option<DispatchedOutcome>,
}

/// Crash-durable dispatch idempotency outbox.
///
/// A trait so the facade ([`crate::facade::ActionRuntime`]) can use either
/// the in-memory implementation ([`InMemoryDispatchOutbox`], the default,
/// which does not survive a crash) or the crash-durable implementation
/// ([`JournalDispatchOutbox`]) without changing its logic. All methods take
/// `&self` (internal mutation behind a lock), so the trait is
/// `dyn`-compatible.
pub trait DispatchOutboxStore: std::fmt::Debug + Send + Sync {
    /// Returns the implementation's **stable kind identifier** (`"in-memory"` or
    /// `"journal"`).
    ///
    /// This is deliberately a small, secret-free hook that lets the caller
    /// (and tests) determine WHICH outbox is wired up without exposing
    /// internal state or a path. A configuration that requires crash
    /// durability can thus confirm that the `"journal"` variant is behind
    /// the `dyn`, not the default `"in-memory"` one. The value is a stable
    /// contract — do not change existing strings.
    fn kind(&self) -> &'static str;

    /// Checks a key's current state **without performing** anything.
    ///
    /// # Errors
    /// For disk-backed implementations, [`ActionError::Proof`] if reading the log fails.
    fn lookup(&self, key: &str) -> Result<DispatchLookup>;

    /// Records the key's **intent** (`DISPATCH_INTENT`) — must be called
    /// **BEFORE** the side effect is executed.
    ///
    /// # Errors
    /// For disk-backed implementations, [`ActionError::Proof`] if the write fails.
    fn record_intent(&self, key: &str) -> Result<()>;

    /// Records the key's **commitment** (`DISPATCH_COMMITTED`) with its
    /// outcome — must be called **only** after the side effect has
    /// executed successfully.
    ///
    /// # Errors
    /// For disk-backed implementations, [`ActionError::Proof`] if the write fails.
    fn record_committed(&self, key: &str, outcome: &DispatchedOutcome) -> Result<()>;
}

/// In-memory outbox (default + test use).
///
/// Fast, but **does not survive a process crash** — state is empty after a
/// restart. This is deliberately the same behavior as before the outbox
/// existed: an in-memory runtime does not provide an exactly-once guarantee
/// across a crash (use [`JournalDispatchOutbox`] in production).
#[derive(Debug, Default)]
pub struct InMemoryDispatchOutbox {
    /// Key → state.
    inner: Mutex<HashMap<String, KeyState>>,
}

impl InMemoryDispatchOutbox {
    /// Creates an empty in-memory outbox.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the internal map, recovering from a poisoned lock without panicking.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, KeyState>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl DispatchOutboxStore for InMemoryDispatchOutbox {
    fn kind(&self) -> &'static str {
        "in-memory"
    }

    fn lookup(&self, key: &str) -> Result<DispatchLookup> {
        let map = self.lock();
        Ok(match map.get(key) {
            None => DispatchLookup::NotStarted,
            Some(state) => match &state.committed {
                Some(outcome) => DispatchLookup::Committed(outcome.clone()),
                None if state.intent => DispatchLookup::InProgress,
                None => DispatchLookup::NotStarted,
            },
        })
    }

    fn record_intent(&self, key: &str) -> Result<()> {
        self.lock().entry(key.to_string()).or_default().intent = true;
        Ok(())
    }

    fn record_committed(&self, key: &str, outcome: &DispatchedOutcome) -> Result<()> {
        let mut map = self.lock();
        let state = map.entry(key.to_string()).or_default();
        state.intent = true;
        state.committed = Some(outcome.clone());
        Ok(())
    }
}

/// A single stored row (intent or committed) of the outbox, in on-disk form.
///
/// A small, secret-free record: key + optional outcome (only on committed rows).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OutboxRow {
    /// The idempotency key.
    key: String,
    /// The outcome (only on committed rows; `None` on intent rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<DispatchedOutcome>,
}

/// Crash-durable outbox on top of [`FileJournal`].
///
/// An append-only log: `dispatch_intent` and `dispatch_committed` markers,
/// from which state is reconstructed by replay. Because
/// [`FileJournal::append`] flushes and fsyncs before returning, a recorded
/// intent/committed is on disk even after an abrupt crash — this is the
/// cornerstone of the whole exactly-once guarantee.
///
/// ## Secrecy invariant
/// Only `OutboxRow`'s secret-free fields (key + identifiers + status) are
/// written to disk. No raw payload and no secrets.
pub struct JournalDispatchOutbox {
    /// Append-only log to which intents and commitments are recorded.
    journal: FileJournal,
    /// The next row's sequence slot (monotonic).
    next_step: Mutex<StepId>,
}

impl std::fmt::Debug for JournalDispatchOutbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalDispatchOutbox")
            .field("path", &self.journal.path())
            .finish_non_exhaustive()
    }
}

impl JournalDispatchOutbox {
    /// Opens (or creates) a crash-durable outbox at the given file path.
    ///
    /// Key state is reconstructed from the existing log immediately, so
    /// after a restart, already-committed dispatches are returned
    /// identically and unfinished ones are detected
    /// ([`DispatchLookup::InProgress`]).
    ///
    /// # Errors
    /// [`ActionError::Proof`] if the journal cannot be opened or read.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let journal = FileJournal::open(path)
            .map_err(|e| ActionError::Proof(format!("open dispatch outbox failed: {e}")))?;
        let len = journal
            .len()
            .map_err(|e| ActionError::Proof(format!("read dispatch outbox failed: {e}")))?;
        let next = StepId::new(u64::try_from(len).unwrap_or(u64::MAX));
        Ok(Self {
            journal,
            next_step: Mutex::new(next),
        })
    }

    /// Returns the log's file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.journal.path()
    }

    /// Reserves and returns the next sequence slot (monotonic).
    fn next_step_id(&self) -> StepId {
        let mut guard = self
            .next_step
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = *guard;
        *guard = current.next();
        current
    }

    /// Appends a marker to the log with the given name and row.
    fn append_marker(&self, name: &str, row: &OutboxRow) -> Result<()> {
        let payload = serde_json::to_value(row)
            .map_err(|e| ActionError::Proof(format!("encode outbox row failed: {e}")))?;
        let entry = JournalEntry::marker(self.next_step_id(), name, payload);
        self.journal
            .append(entry)
            .map_err(|e| ActionError::Proof(format!("append outbox marker failed: {e}")))
    }

    /// Reconstructs the state of a single key by replaying the log.
    ///
    /// Replay walks the rows in order: `dispatch_intent` marks the intent,
    /// `dispatch_committed` stores the outcome. A later committed wins.
    fn replay_key(&self, key: &str) -> Result<KeyState> {
        let entries = self
            .journal
            .replay_all()
            .map_err(|e| ActionError::Proof(format!("replay dispatch outbox failed: {e}")))?;
        let mut state = KeyState::default();
        for entry in entries {
            let EntryKind::Marker { name, payload } = entry.kind else {
                continue;
            };
            let is_intent = name == DISPATCH_INTENT;
            let is_committed = name == DISPATCH_COMMITTED;
            if !is_intent && !is_committed {
                continue;
            }
            let row: OutboxRow = serde_json::from_value(payload)
                .map_err(|e| ActionError::Proof(format!("decode outbox row failed: {e}")))?;
            if row.key != key {
                continue;
            }
            if is_intent {
                state.intent = true;
            } else if let Some(outcome) = row.outcome {
                state.intent = true;
                state.committed = Some(outcome);
            }
        }
        Ok(state)
    }
}

impl DispatchOutboxStore for JournalDispatchOutbox {
    fn kind(&self) -> &'static str {
        "journal"
    }

    fn lookup(&self, key: &str) -> Result<DispatchLookup> {
        let state = self.replay_key(key)?;
        Ok(match state.committed {
            Some(outcome) => DispatchLookup::Committed(outcome),
            None if state.intent => DispatchLookup::InProgress,
            None => DispatchLookup::NotStarted,
        })
    }

    fn record_intent(&self, key: &str) -> Result<()> {
        self.append_marker(
            DISPATCH_INTENT,
            &OutboxRow {
                key: key.to_string(),
                outcome: None,
            },
        )
    }

    fn record_committed(&self, key: &str, outcome: &DispatchedOutcome) -> Result<()> {
        self.append_marker(
            DISPATCH_COMMITTED,
            &OutboxRow {
                key: key.to_string(),
                outcome: Some(outcome.clone()),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// RAII temp file without external crates.
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "familyclaw-outbox-{tag}-{}-{:?}.jsonl",
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

    fn sample_outcome() -> DispatchedOutcome {
        DispatchedOutcome {
            task_id: ActionTaskId::new(),
            status: TaskStatus::NeedsApproval,
            pending_approval: Some(ApprovalId::new()),
            error: None,
        }
    }

    #[test]
    fn in_memory_lookup_lifecycle() {
        let outbox = InMemoryDispatchOutbox::new();
        assert_eq!(
            outbox.lookup("k").expect("lookup"),
            DispatchLookup::NotStarted
        );

        outbox.record_intent("k").expect("intent");
        assert_eq!(
            outbox.lookup("k").expect("lookup"),
            DispatchLookup::InProgress
        );

        let outcome = sample_outcome();
        outbox.record_committed("k", &outcome).expect("commit");
        match outbox.lookup("k").expect("lookup") {
            DispatchLookup::Committed(got) => assert_eq!(got, outcome),
            other => panic!("expected Committed, got {other:?}"),
        }
    }

    #[test]
    fn outcome_roundtrips_through_result() {
        let outcome = sample_outcome();
        let result = outcome.clone().into_result().expect("ok");
        assert_eq!(result.task_id, outcome.task_id);
        assert_eq!(result.pending_approval, outcome.pending_approval);

        let err_outcome = DispatchedOutcome::from_error("boom");
        let err = err_outcome.into_result().expect_err("err");
        assert!(matches!(err, ActionError::ExecutionFailed(_)));
    }

    #[test]
    fn durable_committed_survives_simulated_restart() {
        let tmp = TempPath::new("commit-survives");
        let outcome = sample_outcome();

        // Step 1: record intent + committed, then "crash" (drop).
        {
            let outbox = JournalDispatchOutbox::open(tmp.path()).expect("open 1");
            outbox.record_intent("turn-0-dispatch-0").expect("intent");
            outbox
                .record_committed("turn-0-dispatch-0", &outcome)
                .expect("commit");
        }

        // Step 2: re-open — the committed outcome is returned identically.
        let resumed = JournalDispatchOutbox::open(tmp.path()).expect("open 2");
        match resumed.lookup("turn-0-dispatch-0").expect("lookup") {
            DispatchLookup::Committed(got) => assert_eq!(got, outcome),
            other => panic!("expected Committed after restart, got {other:?}"),
        }
    }

    #[test]
    fn durable_intent_only_is_in_progress_after_restart() {
        let tmp = TempPath::new("intent-only");

        // Step 1: record ONLY intent (simulates a crash mid-side-effect).
        {
            let outbox = JournalDispatchOutbox::open(tmp.path()).expect("open 1");
            outbox.record_intent("turn-0-dispatch-0").expect("intent");
        }

        // Step 2: re-open — intent-only → InProgress (fail-closed).
        let resumed = JournalDispatchOutbox::open(tmp.path()).expect("open 2");
        assert_eq!(
            resumed.lookup("turn-0-dispatch-0").expect("lookup"),
            DispatchLookup::InProgress,
            "intent without committed → InProgress after a crash"
        );
        // An unknown key is still NotStarted.
        assert_eq!(
            resumed.lookup("turn-0-dispatch-9").expect("lookup"),
            DispatchLookup::NotStarted
        );
    }

    #[test]
    fn durable_persisted_form_contains_no_raw_secret() {
        let tmp = TempPath::new("no-secret");
        // The key is derived by the caller, not a secret; still verify that
        // storing the outcome does not leak anything beyond keys/identifiers.
        let outbox = JournalDispatchOutbox::open(tmp.path()).expect("open");
        outbox.record_intent("turn-3-dispatch-2").expect("intent");
        outbox
            .record_committed("turn-3-dispatch-2", &sample_outcome())
            .expect("commit");
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read");
        assert!(on_disk.contains("turn-3-dispatch-2"));
        assert!(!on_disk.contains("sk-"));
        assert!(!on_disk.contains("Bearer "));
    }

    #[test]
    fn separate_keys_are_independent() {
        let outbox = InMemoryDispatchOutbox::new();
        outbox.record_intent("a").expect("a intent");
        outbox
            .record_committed("a", &sample_outcome())
            .expect("a commit");
        // A different key is untouched.
        assert_eq!(
            outbox.lookup("b").expect("lookup"),
            DispatchLookup::NotStarted
        );
    }
}
