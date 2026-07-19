//! Storage surface for pending approvals ([`PendingApprovalStore`]) — Layer A.
//!
//! [`crate::facade::ActionRuntime`] leaves a `write-external` task waiting
//! for human approval: `submit-task` grants a payload-bound approval and
//! stores a pending record, which `approve` later consumes. Previously this
//! record lived in a plain in-process `HashMap` — **a process crash between
//! `submit-task` and `approve` permanently lost the pending approval**,
//! leaving the already-granted action stuck, unable to be either approved or
//! denied.
//!
//! This module abstracts storage behind the [`PendingApprovalStore`] trait
//! and provides two implementations:
//!
//! - [`InMemoryPendingStore`] — a `HashMap` behind the trait. Default + test
//!   use. Fast, but does **not** survive a crash.
//! - [`JournalPendingStore`] — crash-resistant, an append-only log based on
//!   [`familyclaw_durable::FileJournal`]. Every insert and removal is
//!   recorded to disk (flush + fsync), and on restart state is reconstructed
//!   from the log — the pending approval **survives a crash** and is still
//!   approvable.
//!
//! ## Secrecy invariant (absolute)
//! The stored form ([`PendingRecord`]) never contains **the raw payload,
//! secrets, or Layer B data** — only:
//! - the approval and task ids,
//! - the payload's SHA-256 **hash** ([`crate::approval::Approval::payload_hash`]),
//! - a redacted, human-readable summary ([`PendingRecord::redacted_summary`]),
//! - creation and expiry timestamps.
//!
//! Payload binding is preserved through the hash: when `approve` later
//! consumes the approval, the presented payload is re-hashed and compared
//! against the stored hash ([`crate::approval::ApprovalLedger::consume`]).
//! The payload itself can therefore never be read back from disk.
//!
//! ## Capacity cap, TTL eviction, and the rate-limit hook
//! - **Capacity cap** ([`PendingCapacity`]): an insert is rejected
//!   fail-closed ([`ActionError::PolicyDenied`]) once the number pending
//!   already reaches the cap — prevents unbounded memory/disk growth (`DoS`
//!   protection).
//! - **TTL eviction** ([`PendingApprovalStore::evict_expired`]): expired
//!   records are removed using exactly the same fail-closed expiry as
//!   [`crate::approval`] (`now > expires_at`). An expired approval can no
//!   longer be consumed, so keeping it around would be pure garbage.
//! - **Per-being rate limit** ([`DangerousToolRateLimiter`]): a counter for
//!   limiting dangerous (approval-requiring) tool calls with a sliding time
//!   window. **Hooked into the approval path**
//!   ([`crate::facade::ActionRuntime::submit_task`]): when a task would be
//!   left waiting for human approval, the facade first asks this limiter
//!   whether the being still has room — if not, approval is not granted and
//!   the call is rejected fail-closed ([`ActionError::PolicyDenied`]). The
//!   global capacity cap bounds the whole queue; this adds a **per-being**
//!   cap on top of it. Auto-run tasks (read / local write) are not
//!   rate-limited. Deterministic: a timestamp is injected.
//!
//! ## Determinism
//! All time-reading logic takes the timestamp as an injected parameter
//! ([`familyclaw_core::time::Timestamp`]) — the clock is never read inside
//! the module.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use familyclaw_core::time::Timestamp;
use familyclaw_durable::{EntryKind, FileJournal, Journal, JournalEntry, StepId};

use crate::approval::Approval;
use crate::error::{ActionError, Result};
use crate::ids::{ActionTaskId, ApprovalId};

/// Module readiness flag — kept so that [`crate::all_modules_scaffolded`]
/// still compiles alongside the other modules.
pub(crate) const SCAFFOLDED: bool = true;

/// The **secret-free** storage form of a pending approval.
///
/// This is one row in the storage surface: it carries exactly the data
/// `approve` needs to resume a stalled task's execution — the task's id and
/// its payload-bound approval — plus a redacted summary for display to the
/// operator.
///
/// ## Secrecy invariant
/// Field by field:
/// - [`PendingRecord::task_id`] — the task's UUID (not a secret).
/// - [`PendingRecord::approval`] — an [`Approval`], whose only
///   payload-derived field is a SHA-256 **hash** (not the raw payload). The
///   rest are ids, timestamps, and a single-use flag.
/// - [`PendingRecord::redacted_summary`] — a human-readable, redacted
///   summary (e.g. "`github_issue_draft` is awaiting approval"). It is the
///   caller's **responsibility** not to put secrets here; by default it is
///   derived only from the skill's name and ids.
/// - [`PendingRecord::created_at`] — the creation moment (for auditing).
///
/// The raw payload, API keys, tokens, or Layer B data are never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRecord {
    /// The task the approval concerns.
    pub task_id: ActionTaskId,
    /// The payload-bound approval (carries only the payload's **hash**).
    pub approval: Approval,
    /// A redacted, human-readable summary for display to the operator.
    ///
    /// Must not contain the raw payload or secrets — only neutral metadata
    /// (the skill's name, what the approval concerns at a general level).
    pub redacted_summary: String,
    /// The moment the pending record was created (for auditing).
    pub created_at: Timestamp,
}

impl PendingRecord {
    /// Builds a pending record for a task and its payload-bound approval.
    ///
    /// `redacted_summary` is a neutral summary provided by the caller; **it
    /// must not contain secrets** (it is stored to disk as-is).
    #[must_use]
    pub fn new(
        task_id: ActionTaskId,
        approval: Approval,
        redacted_summary: impl Into<String>,
        created_at: Timestamp,
    ) -> Self {
        Self {
            task_id,
            approval,
            redacted_summary: redacted_summary.into(),
            created_at,
        }
    }

    /// The approval's id (the storage surface key).
    #[must_use]
    pub fn approval_id(&self) -> ApprovalId {
        self.approval.id
    }

    /// The moment after which the record is expired (`approval.expires_at`).
    #[must_use]
    pub fn expires_at(&self) -> Timestamp {
        self.approval.expires_at
    }

    /// Whether the record is expired relative to the given moment `now`
    /// (`now > expires_at`).
    ///
    /// Uses exactly the same fail-closed expiry boundary as
    /// [`Approval::is_expired`]: exactly `expires_at` is still valid, a
    /// genuinely later moment is not.
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        self.approval.is_expired(now)
    }
}

/// Capacity cap for the number of pending approvals.
///
/// Prevents unbounded growth of the storage surface (memory/disk): once the
/// number pending already reaches the cap, a new insert is rejected
/// fail-closed. The default is [`PendingCapacity::DEFAULT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingCapacity(usize);

impl PendingCapacity {
    /// The default cap (1024 pending approvals).
    ///
    /// Under a realistic human-in-the-loop load, pending approvals are
    /// usually a handful; a cap of a thousand gives ample margin while still
    /// bounding the `DoS` surface.
    pub const DEFAULT: PendingCapacity = PendingCapacity(1024);

    /// Builds a cap from the given limit.
    ///
    /// A limit of `0` means "no room for even one" — all inserts are
    /// rejected (can be used to disable a subsystem). Use
    /// [`PendingCapacity::DEFAULT`] if you don't need a specific limit.
    #[must_use]
    pub const fn new(limit: usize) -> Self {
        Self(limit)
    }

    /// Returns the cap's numeric value (the maximum allowed number pending).
    #[must_use]
    pub const fn limit(self) -> usize {
        self.0
    }

    /// Whether there is room for one more when the current size is `current`.
    #[must_use]
    pub const fn has_room_for_one_more(self, current: usize) -> bool {
        current < self.0
    }
}

impl Default for PendingCapacity {
    /// The default is [`PendingCapacity::DEFAULT`].
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Per-being rate limit for dangerous (approval-requiring) tool calls.
///
/// A sliding time window: each being (`being`) is allowed at most
/// `max_per_window` records within a `window_secs`-second window (both given
/// to [`DangerousToolRateLimiter::new`]). This is a storage-surface-
/// independent hook that the facade ([`crate::facade::ActionRuntime`]) asks
/// in `submit-task` **before** it grants a new approval — fail-closed
/// protection against a single being flooding the pending queue with
/// dangerous requests. The global capacity cap ([`PendingCapacity`]) bounds
/// the whole queue; this adds a **per-being** cap on top of it.
///
/// ## Determinism
/// A timestamp is injected
/// ([`DangerousToolRateLimiter::check_and_record`]); the clock is never read
/// inside. Expired timestamps are lazily cleaned up during the check.
#[derive(Debug, Default)]
pub struct DangerousToolRateLimiter {
    /// The window length in seconds.
    window_secs: i64,
    /// The maximum allowed number of records in the window per being.
    max_per_window: usize,
    /// Being → recent record timestamps (oldest first).
    hits: Mutex<HashMap<String, VecDeque<Timestamp>>>,
}

impl DangerousToolRateLimiter {
    /// Builds a limiter with the given window and cap.
    ///
    /// `max_per_window = 0` blocks all calls (a hard cutoff). `window_secs
    /// <= 0` is treated as an instantaneous window (in practice every call
    /// is in a new window) — this does not panic, but is fail-open only with
    /// respect to the window; use a positive window for a real limit.
    #[must_use]
    pub fn new(window_secs: i64, max_per_window: usize) -> Self {
        Self {
            window_secs,
            max_per_window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Checks whether the being has room for a new dangerous call, and if
    /// so, **records** it and returns `Ok(())`. If the quota is exhausted,
    /// returns [`ActionError::PolicyDenied`] **without recording** the call
    /// (fail-closed).
    ///
    /// Sliding window: before the check, timestamps older than `now -
    /// window_secs` are evicted. This way the counter only tracks calls
    /// within the window.
    ///
    /// # Errors
    /// [`ActionError::PolicyDenied`] if the being has already used its quota
    /// in this window.
    pub fn check_and_record(&self, being: &str, now: Timestamp) -> Result<()> {
        let cutoff = now - chrono::Duration::seconds(self.window_secs.max(0));
        let mut guard = self
            .hits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = guard.entry(being.to_string()).or_default();
        // Evict timestamps outside the window (oldest first).
        while entry.front().is_some_and(|t| *t < cutoff) {
            entry.pop_front();
        }
        if entry.len() >= self.max_per_window {
            return Err(ActionError::PolicyDenied(format!(
                "vaarallisten työkalukutsujen rate-limit ylittyi olennolle '{being}' \
                 ({} / {} {}s ikkunassa)",
                entry.len(),
                self.max_per_window,
                self.window_secs
            )));
        }
        entry.push_back(now);
        Ok(())
    }

    /// How many records the being has in the window at moment `now`
    /// (evicts expired ones first). Mainly for testing and diagnostics.
    #[must_use]
    pub fn count_in_window(&self, being: &str, now: Timestamp) -> usize {
        let cutoff = now - chrono::Duration::seconds(self.window_secs.max(0));
        let mut guard = self
            .hits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = guard.get_mut(being) else {
            return 0;
        };
        while entry.front().is_some_and(|t| *t < cutoff) {
            entry.pop_front();
        }
        entry.len()
    }
}

/// Storage surface for pending approvals.
///
/// Abstracts **where** pending approvals live — in process memory or on
/// crash-resistant disk — so that [`crate::facade::ActionRuntime`] can swap
/// the storage backend without breaking its logic. All methods take `&self`
/// (internal mutation behind a lock), so the trait is `dyn`-compatible.
///
/// ## Contract
/// - [`insert`](PendingApprovalStore::insert) **honors the capacity cap**:
///   if the surface is already full, the insert is rejected fail-closed
///   ([`ActionError::PolicyDenied`]) and nothing is written.
/// - [`get`](PendingApprovalStore::get) / [`remove`](PendingApprovalStore::remove)
///   return the stored [`PendingRecord`] in its full form (including the
///   payload-bound approval), so `approve` can resume execution.
/// - [`remove`](PendingApprovalStore::remove) is **single-use**: a consumed
///   id can no longer be found (the same nonce semantics as
///   [`crate::approval`]).
/// - [`list`](PendingApprovalStore::list) returns all pending records (used
///   by the operator surface + capacity accounting).
/// - [`evict_expired`](PendingApprovalStore::evict_expired) removes expired
///   records using the fail-closed boundary.
///
/// ## Secrets
/// An implementation that persists to disk may write **only**
/// [`PendingRecord`]'s secret-free fields (hash + ids + redacted summary) —
/// never the raw payload.
pub trait PendingApprovalStore: Send + Sync {
    /// Inserts a pending record, **if** the capacity cap is not exceeded.
    ///
    /// The key is `record.approval_id()`. Re-inserting the same id replaces
    /// the prior one (in practice ids are unique). An insert is counted
    /// against capacity only when it is a **new** id.
    ///
    /// # Errors
    /// [`ActionError::PolicyDenied`] if the capacity cap
    /// ([`PendingCapacity`]) prevents adding a new id. On disk
    /// implementations, also an I/O error ([`ActionError::Proof`]) if
    /// writing to the journal fails.
    fn insert(&self, record: PendingRecord) -> Result<()>;

    /// Looks up a pending record by approval id; `None` if not found (or
    /// already consumed/evicted).
    ///
    /// Returns a copy of the whole record (not a reference), so the
    /// implementation can hold its internal lock only for the duration of
    /// the lookup.
    ///
    /// # Errors
    /// On disk implementations, [`ActionError::Proof`] if reading the log
    /// fails.
    fn get(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>>;

    /// Removes (consumes) a pending record and returns it, if it existed;
    /// `None` if it did not.
    ///
    /// Single-use: after removal the same id can no longer be found via
    /// [`get`](PendingApprovalStore::get). On disk implementations, removal
    /// is permanent (across a crash).
    ///
    /// # Errors
    /// On disk implementations, [`ActionError::Proof`] if writing the
    /// removal record fails.
    fn remove(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>>;

    /// The number of pending records.
    ///
    /// # Errors
    /// On disk implementations, [`ActionError::Proof`] if reading the log
    /// fails.
    fn len(&self) -> Result<usize>;

    /// Whether the surface is empty (no pending records at all).
    ///
    /// # Errors
    /// Same as [`len`](PendingApprovalStore::len).
    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Lists all pending records.
    ///
    /// Used both by the operator surface ([`crate::facade::ActionRuntime::pending_approvals`])
    /// and by capacity-cap accounting. Order is not guaranteed; the caller
    /// stabilizes it if needed (e.g. by approval id).
    ///
    /// # Errors
    /// On disk implementations, [`ActionError::Proof`] if reading the log
    /// fails.
    fn list(&self) -> Result<Vec<PendingRecord>>;

    /// Removes all records expired as of the given moment `now` and returns
    /// the number evicted.
    ///
    /// Uses exactly the same fail-closed expiry boundary as
    /// [`crate::approval`] ([`PendingRecord::is_expired`]): `now >
    /// expires_at`. An expired approval can no longer be consumed, so
    /// keeping it around would be pure garbage on the storage surface.
    ///
    /// # Errors
    /// On disk implementations, [`ActionError::Proof`] if reading/writing
    /// the log fails.
    fn evict_expired(&self, now: Timestamp) -> Result<usize>;

    /// Returns the surface's **kind tag** (`"in-memory"` or `"journal"`).
    ///
    /// This is a secret-free check hook for the assembler and tests: it
    /// lets you confirm that a persistent configuration got the
    /// crash-resistant (`"journal"`) pending-approvals surface instead of
    /// the default in-memory (`"in-memory"`) one, without exposing internal
    /// state or the file path. Same purpose as
    /// [`crate::dispatch_outbox::DispatchOutboxStore::kind`]. The default is
    /// `"in-memory"`; crash-resistant implementations override this.
    fn kind(&self) -> &'static str {
        "in-memory"
    }
}

/// In-memory storage surface ([`HashMap`] behind the trait).
///
/// Default and test use: fast and simple, **but does not survive a process
/// crash** — on a crash all pending approvals are lost. In production where
/// crash resistance is a requirement, use [`JournalPendingStore`].
#[derive(Debug)]
pub struct InMemoryPendingStore {
    /// Approval id → pending record.
    inner: Mutex<HashMap<ApprovalId, PendingRecord>>,
    /// The capacity cap.
    capacity: PendingCapacity,
}

impl InMemoryPendingStore {
    /// Creates an empty in-memory surface with the default capacity
    /// ([`PendingCapacity::DEFAULT`]).
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(PendingCapacity::DEFAULT)
    }

    /// Creates an empty in-memory surface with the given capacity cap.
    #[must_use]
    pub fn with_capacity(capacity: PendingCapacity) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            capacity,
        }
    }

    /// Locks the internal map, recovering from a poisoned lock without panicking.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ApprovalId, PendingRecord>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Default for InMemoryPendingStore {
    /// The default is an empty surface with the default capacity.
    fn default() -> Self {
        Self::new()
    }
}

impl PendingApprovalStore for InMemoryPendingStore {
    fn insert(&self, record: PendingRecord) -> Result<()> {
        let mut map = self.lock();
        let id = record.approval_id();
        // Capacity is counted only for NEW ids: replacing an existing one
        // does not grow the size.
        if !map.contains_key(&id) && !self.capacity.has_room_for_one_more(map.len()) {
            return Err(ActionError::PolicyDenied(format!(
                "odottavien hyväksyntöjen kapasiteettikatto {} täynnä",
                self.capacity.limit()
            )));
        }
        map.insert(id, record);
        Ok(())
    }

    fn get(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>> {
        Ok(self.lock().get(&approval_id).cloned())
    }

    fn remove(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>> {
        Ok(self.lock().remove(&approval_id))
    }

    fn len(&self) -> Result<usize> {
        Ok(self.lock().len())
    }

    fn list(&self) -> Result<Vec<PendingRecord>> {
        Ok(self.lock().values().cloned().collect())
    }

    fn evict_expired(&self, now: Timestamp) -> Result<usize> {
        let mut map = self.lock();
        let before = map.len();
        map.retain(|_, record| !record.is_expired(now));
        Ok(before - map.len())
    }
}

/// The journal row's logical name for a pending-record insert.
const PENDING_PUT: &str = "pending_approval_put";
/// The journal row's logical name for a pending-record removal (tombstone).
const PENDING_DELETE: &str = "pending_approval_delete";

/// Default compaction factor: the log is compacted automatically when the
/// number of physical rows exceeds `AUTO_COMPACT_FACTOR *
/// number_of_live_records`.
///
/// A factor of 2 means "compact once at least half the rows are dead"
/// (removed or replaced). This bounds the accumulation of dead rows to a
/// constant factor per live record, so the log's size and the replay's O(n)
/// cost stay on the order of the live state's size instead of growing
/// unboundedly.
const AUTO_COMPACT_FACTOR: usize = 2;

/// The minimum physical row count at which auto-compaction is even
/// considered.
///
/// Prevents pointless compaction on small logs (e.g. 1 live + 1 tombstone =
/// 2 rows would otherwise trigger it immediately). Only once there are this
/// many rows does the dead-row ratio start being monitored.
const AUTO_COMPACT_MIN_ROWS: usize = 64;

/// Crash-resistant storage surface on top of [`familyclaw_durable::FileJournal`].
///
/// An append-only log: every insert is written as a `pending_approval_put`
/// marker (contains the whole secret-free [`PendingRecord`]) and every
/// removal as a `pending_approval_delete` marker (contains only the
/// approval id, a tombstone). State is reconstructed by replaying the log:
/// a later row wins, so a removal undoes an earlier insert.
///
/// Because [`FileJournal::append`] flushes and fsyncs before returning, a
/// completed insert/removal is on disk even after an abrupt crash. When
/// opened, `FileJournal` repairs any incomplete trailing row left by a
/// crash, so the log remains readable. This is how **a pending approval
/// survives a crash between `submit-task` and `approve`**.
///
/// ## Compaction — keeping unbounded growth in check
/// Because the log is append-only, every removal
/// ([`remove`](PendingApprovalStore::remove) /
/// [`evict_expired`](PendingApprovalStore::evict_expired)) and every
/// replacement of the same id leaves **dead rows** in the log: the state is
/// still correct (a later row wins on replay), but the file grows without
/// bound and replay becomes O(n) in the row count — not in the number of
/// live records. [`compact`](JournalPendingStore::compact) rewrites the log
/// to contain **only the live records** (dead/tombstoned/replaced rows are
/// dropped) atomically via [`FileJournal::rewrite`] — the live state is
/// preserved bit-for-bit, and an interruption never loses live rows
/// (rename-based swap). Compaction is triggered either by an operator call
/// ([`compact`](JournalPendingStore::compact)) or **automatically** during
/// an insert or eviction when the fraction of dead rows exceeds a threshold
/// (see `AUTO_COMPACT_FACTOR` and
/// [`with_auto_compact_factor`](JournalPendingStore::with_auto_compact_factor)).
///
/// ## Secrecy invariant
/// Only [`PendingRecord`]'s secret-free fields (the payload's hash, ids, the
/// redacted summary, timestamps) are written to disk — never the raw
/// payload. Compaction preserves this: the rewritten log contains the same
/// secret-free `pending_approval_put` rows.
pub struct JournalPendingStore {
    /// The append-only log to which inserts and removals are recorded.
    journal: FileJournal,
    /// The next row's sequence position (monotonic).
    next_step: Mutex<StepId>,
    /// The capacity cap.
    capacity: PendingCapacity,
    /// Auto-compaction factor: compact when `rows > factor * live`.
    /// `0` disables auto-compaction (manual `compact` only).
    auto_compact_factor: usize,
}

impl std::fmt::Debug for JournalPendingStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalPendingStore")
            .field("path", &self.journal.path())
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl JournalPendingStore {
    /// Opens (or creates) a crash-resistant surface from the given file
    /// path with the default capacity.
    ///
    /// Pending approvals are reconstructed immediately from an existing
    /// log, so after a restart they are still retrievable via
    /// [`get`](PendingApprovalStore::get) and approvable.
    ///
    /// # Errors
    /// [`ActionError::Proof`] if the journal cannot be opened or reading it
    /// (to infer the sequence position) fails.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_capacity(path, PendingCapacity::DEFAULT)
    }

    /// Opens (or creates) a surface with the given capacity cap.
    ///
    /// # Errors
    /// [`ActionError::Proof`] if the journal cannot be opened or read.
    pub fn open_with_capacity(path: impl AsRef<Path>, capacity: PendingCapacity) -> Result<Self> {
        let journal = FileJournal::open(path)
            .map_err(|e| ActionError::Proof(format!("open pending journal failed: {e}")))?;
        // Infer the next sequence position from the existing log's length.
        let len = journal
            .len()
            .map_err(|e| ActionError::Proof(format!("read pending journal failed: {e}")))?;
        let next = StepId::new(u64::try_from(len).unwrap_or(u64::MAX));
        Ok(Self {
            journal,
            next_step: Mutex::new(next),
            capacity,
            auto_compact_factor: AUTO_COMPACT_FACTOR,
        })
    }

    /// Sets the auto-compaction factor (chainable).
    ///
    /// The log is compacted automatically when the number of physical rows
    /// exceeds `factor * number_of_live_records` (and there are at least
    /// `AUTO_COMPACT_MIN_ROWS` rows). The default is `AUTO_COMPACT_FACTOR`
    /// (2). A value of `0` **disables** auto-compaction — the log is then
    /// compacted only via a [`compact`](Self::compact) call.
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
            .map_err(|e| ActionError::Proof(format!("append pending marker failed: {e}")))
    }

    /// Reconstructs the current state (approval id → record) by replaying the log.
    ///
    /// The replay processes rows in order: `pending_approval_put`
    /// inserts/replaces the record, `pending_approval_delete` removes it
    /// (tombstone). Other rows are skipped. This way a later row wins and a
    /// removal undoes an insert.
    fn replay_state(&self) -> Result<HashMap<ApprovalId, PendingRecord>> {
        let entries = self
            .journal
            .replay_all()
            .map_err(|e| ActionError::Proof(format!("replay pending journal failed: {e}")))?;
        Self::reconstruct_state(entries)
    }

    /// Builds the current state from the given journal rows (a pure
    /// function, no I/O).
    ///
    /// The replay processes rows in order: `pending_approval_put`
    /// inserts/replaces the record, `pending_approval_delete` removes it
    /// (tombstone). Other rows are skipped. A later row wins, so a removal
    /// undoes an earlier insert. Separated from
    /// [`replay_state`](Self::replay_state) so that both the log-reading
    /// replay and [`compact`](Self::compact)'s
    /// [`FileJournal::compact_with`] closure can build state with **the same
    /// logic** — the latter receives the rows already read from under the
    /// lock, and must not read the journal again (deadlock).
    fn reconstruct_state(entries: Vec<JournalEntry>) -> Result<HashMap<ApprovalId, PendingRecord>> {
        let mut state: HashMap<ApprovalId, PendingRecord> = HashMap::new();
        for entry in entries {
            let EntryKind::Marker { name, payload } = entry.kind else {
                continue;
            };
            match name.as_str() {
                PENDING_PUT => {
                    let record: PendingRecord = serde_json::from_value(payload).map_err(|e| {
                        ActionError::Proof(format!("decode pending put record failed: {e}"))
                    })?;
                    state.insert(record.approval_id(), record);
                }
                PENDING_DELETE => {
                    let id: ApprovalId = serde_json::from_value(payload).map_err(|e| {
                        ActionError::Proof(format!("decode pending delete id failed: {e}"))
                    })?;
                    state.remove(&id);
                }
                _ => {}
            }
        }
        Ok(state)
    }

    /// The number of physical journal rows (live + dead). This is the
    /// number the dead-row ratio is measured against; differs from
    /// [`len`](PendingApprovalStore::len), which returns only the number of
    /// live records.
    fn physical_row_count(&self) -> Result<usize> {
        self.journal
            .len()
            .map_err(|e| ActionError::Proof(format!("read pending journal len failed: {e}")))
    }

    /// Rewrites the log to contain **only the live records** (compaction),
    /// dropping all dead rows (tombstones and replaced `put` rows). Returns
    /// the number of dead rows dropped.
    ///
    /// The live state is preserved bit-for-bit: after compaction exactly the
    /// same approvals are retrievable via [`get`](PendingApprovalStore::get),
    /// and reloading (restart) reconstructs identical state. Compaction is
    /// **atomic** ([`FileJournal::rewrite`]: temp + fsync + rename) — if the
    /// process crashes mid-operation, the live file is still in its intact
    /// old state and no live approval is lost.
    ///
    /// Rows are renumbered into a tight `0..N` sequence, and the internal
    /// sequence cursor is set to match, so future inserts continue from the
    /// right place.
    ///
    /// # Errors
    /// [`ActionError::Proof`] if reading the log, serializing rows, or the
    /// atomic rewrite fails. On error the live log is left unchanged
    /// (rewrite does not touch the live file until the temp file is intact).
    pub fn compact(&self) -> Result<usize> {
        // Atomic compaction against appends: [`FileJournal::compact_with`]
        // holds the same file lock for the whole read→filter→swap operation, so
        // a concurrent insert/removal cannot land in a gap and be lost
        // (TOCTOU fix). The `build` closure receives the rows already read,
        // reconstructs the live state, and returns the renumbered live PENDING_PUT rows.
        //
        // Setting the sequence cursor is done SEPARATELY outside the closure: the
        // closure must NOT lock the journal again, but `next_step` is a different
        // mutex than the file lock, so updating it from inside the closure WOULD be
        // safe — but it is still done AFTER `compact_with` RETURNS, so the cursor
        // only updates once the swap has actually succeeded.
        // The live row count is smuggled out of the closure via a `Cell`, so the
        // sequence cursor can be set after the swap (the closure must NOT lock
        // the journal again → it cannot read the cursor via its own path).
        let live_count = std::cell::Cell::new(0usize);
        let dropped = self
            .journal
            .compact_with(|entries| {
                // Reconstruct the live state from the rows already read (same
                // logic as in replay, but WITHOUT re-reading — re-reading would
                // lock the journal and deadlock). The ActionError is wrapped as
                // DurableError text so the type fits compact_with's contract.
                let state = Self::reconstruct_state(entries).map_err(|e| {
                    familyclaw_durable::DurableError::step_failed(
                        "compact_reconstruct",
                        e.to_string(),
                    )
                })?;
                // One PENDING_PUT row per live record, renumbered 0..N.
                let mut kept = Vec::with_capacity(state.len());
                let mut step = StepId::ZERO;
                for record in state.values() {
                    let payload = serde_json::to_value(record)?;
                    kept.push(JournalEntry::marker(step, PENDING_PUT, payload));
                    step = step.next();
                }
                live_count.set(kept.len());
                Ok(kept)
            })
            .map_err(|e| ActionError::Proof(format!("compact pending journal failed: {e}")))?;

        // Point the sequence cursor past the end of the compacted log (= the
        // live count, since rows were renumbered tightly as 0..N).
        {
            let mut guard = self
                .next_step
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = StepId::new(u64::try_from(live_count.get()).unwrap_or(u64::MAX));
        }

        Ok(dropped)
    }

    /// Compacts the log **if** the fraction of dead rows exceeds the threshold.
    ///
    /// Trigger condition: `auto_compact_factor > 0` AND there are at least
    /// [`AUTO_COMPACT_MIN_ROWS`] physical rows AND `rows > factor * live`.
    /// Otherwise does nothing. Called after an insert and after an eviction,
    /// so dead rows don't accumulate without bound. A failure of
    /// auto-compaction **does not** fail the caller: the data is already
    /// safely in the log, so compaction is a pure optimization — the error
    /// is swallowed (the log just stays uncompacted this time).
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
            // Compact; the error is swallowed (data is already in the log, compaction is an optimization).
            let _ = self.compact();
        }
    }
}

impl PendingApprovalStore for JournalPendingStore {
    fn insert(&self, record: PendingRecord) -> Result<()> {
        // Capacity is checked against the reconstructed state; a new id
        // doesn't fit if the surface is already full (replacing an existing one is allowed).
        let state = self.replay_state()?;
        let id = record.approval_id();
        if !state.contains_key(&id) && !self.capacity.has_room_for_one_more(state.len()) {
            return Err(ActionError::PolicyDenied(format!(
                "odottavien hyväksyntöjen kapasiteettikatto {} täynnä",
                self.capacity.limit()
            )));
        }
        let payload = serde_json::to_value(&record)
            .map_err(|e| ActionError::Proof(format!("encode pending record failed: {e}")))?;
        self.append_marker(PENDING_PUT, payload)?;
        // A replacement left a dead row (the old put) → consider auto-compaction.
        self.maybe_auto_compact();
        Ok(())
    }

    fn get(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>> {
        Ok(self.replay_state()?.remove(&approval_id))
    }

    fn remove(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>> {
        let existing = self.replay_state()?.remove(&approval_id);
        if existing.is_some() {
            // Record a tombstone only if the record existed — avoids a pointless row.
            let payload = serde_json::to_value(approval_id)
                .map_err(|e| ActionError::Proof(format!("encode pending delete id failed: {e}")))?;
            self.append_marker(PENDING_DELETE, payload)?;
            // A tombstone is a dead row → consider auto-compaction.
            self.maybe_auto_compact();
        }
        Ok(existing)
    }

    fn len(&self) -> Result<usize> {
        Ok(self.replay_state()?.len())
    }

    fn list(&self) -> Result<Vec<PendingRecord>> {
        Ok(self.replay_state()?.into_values().collect())
    }

    fn evict_expired(&self, now: Timestamp) -> Result<usize> {
        let state = self.replay_state()?;
        let expired: Vec<ApprovalId> = state
            .values()
            .filter(|record| record.is_expired(now))
            .map(PendingRecord::approval_id)
            .collect();
        for id in &expired {
            let payload = serde_json::to_value(id)
                .map_err(|e| ActionError::Proof(format!("encode pending delete id failed: {e}")))?;
            self.append_marker(PENDING_DELETE, payload)?;
        }
        if !expired.is_empty() {
            // The eviction produced tombstones (dead rows) → consider compaction.
            self.maybe_auto_compact();
        }
        Ok(expired.len())
    }

    /// Crash-resistant surface: `"journal"`.
    fn kind(&self) -> &'static str {
        "journal"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::sha256_hex;
    use crate::ids::ActionId;
    use chrono::Duration;
    use familyclaw_core::time::from_unix_secs;
    use std::path::PathBuf;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    /// Helper: a payload-bound approval with the given TTL.
    fn approval_at(now: Timestamp, ttl: Duration) -> Approval {
        let payload =
            serde_json::to_vec(&serde_json::json!({ "to": "general" })).expect("serialize payload");
        Approval {
            id: ApprovalId::new(),
            action_id: ActionId::new(),
            payload_hash: sha256_hex(&payload),
            granted_at: now,
            expires_at: now + ttl,
            consumed: false,
        }
    }

    /// Helper: a pending record with the given TTL.
    fn record_at(now: Timestamp, ttl: Duration) -> PendingRecord {
        PendingRecord::new(
            ActionTaskId::new(),
            approval_at(now, ttl),
            "github_issue_draft odottaa hyväksyntää",
            now,
        )
    }

    /// RAII temp file without external crates.
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let unique = format!(
                "familyclaw-pending-{tag}-{}-{:?}.jsonl",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            );
            p.push(unique);
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

    // ---- In-memory: insert + get + remove ----

    #[test]
    fn in_memory_insert_get_remove_roundtrip() {
        let store = InMemoryPendingStore::new();
        let now = at(1_700_000_000);
        let record = record_at(now, Duration::minutes(60));
        let id = record.approval_id();
        let task_id = record.task_id;

        store.insert(record).expect("insert");
        assert_eq!(store.len().expect("len"), 1);

        let got = store.get(id).expect("get ok").expect("present");
        assert_eq!(got.approval_id(), id);
        assert_eq!(got.task_id, task_id);

        let removed = store.remove(id).expect("remove ok").expect("was present");
        assert_eq!(removed.approval_id(), id);
        // Single-use: after removal it can no longer be found.
        assert!(store.get(id).expect("get ok").is_none());
        assert!(store.remove(id).expect("remove ok").is_none());
        assert!(store.is_empty().expect("empty"));
    }

    #[test]
    fn in_memory_get_missing_is_none() {
        let store = InMemoryPendingStore::new();
        assert!(store.get(ApprovalId::new()).expect("get").is_none());
    }

    #[test]
    fn in_memory_list_returns_all() {
        let store = InMemoryPendingStore::new();
        let now = at(1_700_000_000);
        for _ in 0..3 {
            store
                .insert(record_at(now, Duration::minutes(60)))
                .expect("insert");
        }
        assert_eq!(store.list().expect("list").len(), 3);
    }

    // ---- Capacity cap ----

    #[test]
    fn capacity_cap_rejects_beyond_limit() {
        let store = InMemoryPendingStore::with_capacity(PendingCapacity::new(2));
        let now = at(1_700_000_000);

        store
            .insert(record_at(now, Duration::minutes(60)))
            .expect("first fits");
        store
            .insert(record_at(now, Duration::minutes(60)))
            .expect("second fits");

        // The third exceeds the cap → fail-closed.
        let err = store
            .insert(record_at(now, Duration::minutes(60)))
            .expect_err("third exceeds cap");
        assert!(matches!(err, ActionError::PolicyDenied(_)));
        assert_eq!(store.len().expect("len"), 2);
    }

    #[test]
    fn capacity_cap_allows_replacing_existing_id() {
        let store = InMemoryPendingStore::with_capacity(PendingCapacity::new(1));
        let now = at(1_700_000_000);
        let record = record_at(now, Duration::minutes(60));
        let id = record.approval_id();

        store.insert(record.clone()).expect("first");
        // Re-inserting the same id doesn't grow the size → doesn't breach the cap.
        store.insert(record).expect("replace same id under cap");
        assert_eq!(store.len().expect("len"), 1);
        // The same id is still retrievable after the replacement.
        assert!(store.get(id).expect("get").is_some());
    }

    // ---- TTL eviction ----

    #[test]
    fn ttl_eviction_drops_expired_only() {
        let store = InMemoryPendingStore::new();
        let now = at(1_700_000_000);

        // One expires in 60s, the other in 3600s.
        let short = record_at(now, Duration::seconds(60));
        let long = record_at(now, Duration::seconds(3600));
        let short_id = short.approval_id();
        let long_id = long.approval_id();
        store.insert(short).expect("insert short");
        store.insert(long).expect("insert long");

        // now + 120s: the short one is expired, the long one isn't.
        let evicted = store.evict_expired(at(1_700_000_120)).expect("evict");
        assert_eq!(evicted, 1);
        assert!(store.get(short_id).expect("get").is_none());
        assert!(store.get(long_id).expect("get").is_some());
    }

    #[test]
    fn ttl_eviction_boundary_keeps_exactly_at_expiry() {
        let store = InMemoryPendingStore::new();
        let now = at(1_700_000_000);
        let record = record_at(now, Duration::seconds(60));
        let id = record.approval_id();
        store.insert(record).expect("insert");

        // Exactly expires_at (now+60) is NOT expired (same fail-closed boundary as approval.rs).
        assert_eq!(store.evict_expired(at(1_700_000_060)).expect("evict"), 0);
        assert!(store.get(id).expect("get").is_some());
        // One second past the boundary → evicted.
        assert_eq!(store.evict_expired(at(1_700_000_061)).expect("evict"), 1);
        assert!(store.get(id).expect("get").is_none());
    }

    // ---- Durable: reload across simulated restart ----

    #[test]
    fn durable_reloads_pending_after_simulated_restart() {
        let tmp = TempPath::new("reload");
        let now = at(1_700_000_000);
        let record = record_at(now, Duration::minutes(60));
        let id = record.approval_id();
        let task_id = record.task_id;
        let payload_hash = record.approval.payload_hash.clone();

        // Step 1: write a record to the surface and DROP it (simulates a crash).
        {
            let store = JournalPendingStore::open(tmp.path()).expect("open 1");
            store.insert(record).expect("insert");
            assert_eq!(store.len().expect("len"), 1);
        } // the store is dropped = the process "crashes"

        // Step 2: create the surface AGAIN from the same file — the record survived.
        let resumed = JournalPendingStore::open(tmp.path()).expect("open 2");
        assert_eq!(resumed.len().expect("len"), 1, "pending survived restart");
        let got = resumed.get(id).expect("get").expect("still present");
        assert_eq!(got.approval_id(), id);
        assert_eq!(got.task_id, task_id);
        // Payload binding survived: the hash is still the same (approve can consume it).
        assert_eq!(got.approval.payload_hash, payload_hash);
        assert!(!got.approval.consumed, "not yet consumed → approvable");

        // Approvable: removal consumes it permanently.
        let removed = resumed.remove(id).expect("remove").expect("present");
        assert_eq!(removed.approval_id(), id);

        // Step 3: one more restart — the removal also survived (tombstone).
        let after_remove = JournalPendingStore::open(tmp.path()).expect("open 3");
        assert!(after_remove.get(id).expect("get").is_none());
        assert!(after_remove.is_empty().expect("empty"));
    }

    #[test]
    fn durable_persisted_form_contains_no_raw_secret() {
        let tmp = TempPath::new("no-secret");
        let now = at(1_700_000_000);

        // Build a record whose payload CONTAINED a secret — but only the hash
        // is stored, not the raw value.
        let secret = format!("sk-{}", "live".repeat(4));
        let payload =
            serde_json::to_vec(&serde_json::json!({ "api_key": secret })).expect("serialize");
        let approval = Approval {
            id: ApprovalId::new(),
            action_id: ActionId::new(),
            payload_hash: sha256_hex(&payload),
            granted_at: now,
            expires_at: now + Duration::minutes(60),
            consumed: false,
        };
        let record = PendingRecord::new(
            ActionTaskId::new(),
            approval,
            "skill odottaa hyväksyntää",
            now,
        );

        let store = JournalPendingStore::open(tmp.path()).expect("open");
        store.insert(record).expect("insert");

        // The raw text read from disk must NOT contain the secret.
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read journal file");
        assert!(
            !on_disk.contains(&secret),
            "persisted journal must never contain the raw secret"
        );
        assert!(!on_disk.contains("sk-livelivelivelive"));
        // But the hash IS present (payload binding is preserved).
        assert!(on_disk.contains(&sha256_hex(&payload)));
    }

    #[test]
    fn durable_capacity_cap_rejects_beyond_limit() {
        let tmp = TempPath::new("cap");
        let now = at(1_700_000_000);
        let store = JournalPendingStore::open_with_capacity(tmp.path(), PendingCapacity::new(1))
            .expect("open");

        store
            .insert(record_at(now, Duration::minutes(60)))
            .expect("first fits");
        let err = store
            .insert(record_at(now, Duration::minutes(60)))
            .expect_err("second exceeds cap");
        assert!(matches!(err, ActionError::PolicyDenied(_)));
        assert_eq!(store.len().expect("len"), 1);
    }

    #[test]
    fn durable_evict_expired_persists_across_restart() {
        let tmp = TempPath::new("evict");
        let now = at(1_700_000_000);
        let short = record_at(now, Duration::seconds(60));
        let short_id = short.approval_id();
        let long = record_at(now, Duration::seconds(3600));
        let long_id = long.approval_id();

        {
            let store = JournalPendingStore::open(tmp.path()).expect("open 1");
            store.insert(short).expect("insert short");
            store.insert(long).expect("insert long");
            let evicted = store.evict_expired(at(1_700_000_120)).expect("evict");
            assert_eq!(evicted, 1);
        }
        // Restart: the eviction persisted.
        let resumed = JournalPendingStore::open(tmp.path()).expect("open 2");
        assert!(resumed.get(short_id).expect("get").is_none());
        assert!(resumed.get(long_id).expect("get").is_some());
        assert_eq!(resumed.len().expect("len"), 1);
    }

    // ---- Compaction ----

    /// Counts physical (live + dead) journal rows by reading the file.
    fn physical_rows(path: &Path) -> usize {
        std::fs::read_to_string(path)
            .map_or(0, |s| s.lines().filter(|l| !l.trim().is_empty()).count())
    }

    #[test]
    fn compact_drops_dead_rows_keeps_live_entries() {
        let tmp = TempPath::new("compact-basic");
        let now = at(1_700_000_000);
        // Auto-compaction turned off, so compaction is controlled manually.
        let store = JournalPendingStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        // Insert N records, remove half → dead rows accumulate.
        let mut ids = Vec::new();
        for _ in 0..10 {
            let record = record_at(now, Duration::minutes(60));
            ids.push(record.approval_id());
            store.insert(record).expect("insert");
        }
        // Remove the first 5 (10 put + 5 delete = 15 physical rows).
        for id in ids.iter().take(5) {
            store.remove(*id).expect("remove");
        }
        assert_eq!(physical_rows(tmp.path()), 15, "10 put + 5 tombstone");
        assert_eq!(store.len().expect("len"), 5, "5 live remain");

        // Compact: 10 dead rows (5 removed puts + 5 tombstones) are dropped.
        let dropped = store.compact().expect("compact");
        assert_eq!(dropped, 10, "15 rows → 5 live rows = 10 dropped");
        assert_eq!(
            physical_rows(tmp.path()),
            5,
            "only live rows remain on disk"
        );
        assert_eq!(store.len().expect("len"), 5, "live count unchanged");

        // All live records are still retrievable, removed ones aren't.
        for id in ids.iter().take(5) {
            assert!(store.get(*id).expect("get").is_none(), "removed gone");
        }
        for id in ids.iter().skip(5) {
            assert!(store.get(*id).expect("get").is_some(), "live still present");
        }
    }

    #[test]
    fn compact_preserves_exact_state_across_reload() {
        let tmp = TempPath::new("compact-reload");
        let now = at(1_700_000_000);

        let live_ids = {
            let store = JournalPendingStore::open(tmp.path())
                .expect("open 1")
                .with_auto_compact_factor(0);
            let mut ids = Vec::new();
            for _ in 0..6 {
                let record = record_at(now, Duration::minutes(60));
                ids.push(record.approval_id());
                store.insert(record).expect("insert");
            }
            // Remove three.
            for id in ids.iter().take(3) {
                store.remove(*id).expect("remove");
            }
            store.compact().expect("compact");
            ids
        };

        // Restart from just the compacted file → identical state.
        let resumed = JournalPendingStore::open(tmp.path()).expect("open 2");
        assert_eq!(
            resumed.len().expect("len"),
            3,
            "3 live survive compaction+reload"
        );
        for id in live_ids.iter().take(3) {
            assert!(
                resumed.get(*id).expect("get").is_none(),
                "removed stay removed"
            );
        }
        for id in live_ids.iter().skip(3) {
            assert!(resumed.get(*id).expect("get").is_some(), "live stay live");
        }
        // Only live rows on disk.
        assert_eq!(physical_rows(tmp.path()), 3);
    }

    #[test]
    fn compact_is_atomic_temp_then_rename() {
        // Compaction must NOT leave a temp file lying around nor corrupt the live
        // file: rewrite writes to a temp file, fsyncs, and only then renames
        // atomically. Every row after compaction is intact JSON.
        let tmp = TempPath::new("compact-atomic");
        let now = at(1_700_000_000);
        let store = JournalPendingStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        let mut ids = Vec::new();
        for _ in 0..8 {
            let record = record_at(now, Duration::minutes(60));
            ids.push(record.approval_id());
            store.insert(record).expect("insert");
        }
        for id in &ids {
            store.remove(*id).expect("remove");
        }
        // Add one live record back.
        let live = record_at(now, Duration::minutes(60));
        let live_id = live.approval_id();
        store.insert(live).expect("insert live");

        store.compact().expect("compact");

        // Every row on disk parses as intact (no half-written row from renaming).
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read");
        for line in on_disk.lines().filter(|l| !l.trim().is_empty()) {
            serde_json::from_str::<JournalEntry>(line).expect("intact json line");
        }
        // Only the live record remains, retrievable.
        assert_eq!(store.len().expect("len"), 1);
        assert!(store.get(live_id).expect("get").is_some());

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
    fn compact_drops_expired_entries() {
        // Compaction itself does NOT evict expired records, but evict_expired
        // tombstones them and a subsequent compaction drops both the tombstones
        // and the dead put rows — so expired records disappear from disk on compaction.
        let tmp = TempPath::new("compact-expired");
        let now = at(1_700_000_000);
        let store = JournalPendingStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        let short = record_at(now, Duration::seconds(60));
        let short_id = short.approval_id();
        let long = record_at(now, Duration::seconds(3600));
        let long_id = long.approval_id();
        store.insert(short).expect("insert short");
        store.insert(long).expect("insert long");

        // Evict the expired one → tombstone. Then compact.
        assert_eq!(store.evict_expired(at(1_700_000_120)).expect("evict"), 1);
        store.compact().expect("compact");

        // The expired one is gone from both state and disk; the valid one survives.
        assert!(store.get(short_id).expect("get").is_none());
        assert!(store.get(long_id).expect("get").is_some());
        assert_eq!(physical_rows(tmp.path()), 1, "only the live entry remains");
        // The expired record's hash/id no longer appears on disk.
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read");
        assert!(
            !on_disk.contains(&short_id.to_string()),
            "expired id gone from disk"
        );
    }

    #[test]
    fn auto_compact_triggers_when_dead_rows_exceed_threshold() {
        // The default factor (2) + an insert/remove cycle grows dead rows,
        // until auto-compaction triggers and shrinks the log to the live state's size.
        let tmp = TempPath::new("auto-compact");
        let now = at(1_700_000_000);
        let store = JournalPendingStore::open(tmp.path()).expect("open"); // default factor

        // Keep only a few live, but do many insert/remove pairs → once
        // physical rows > 2*live AND >= AUTO_COMPACT_MIN_ROWS, compaction triggers.
        // One permanent live record:
        let keeper = record_at(now, Duration::minutes(60));
        let keeper_id = keeper.approval_id();
        store.insert(keeper).expect("insert keeper");

        // 100 insert+remove pairs = 200 dead rows if there were no compaction.
        for _ in 0..100 {
            let r = record_at(now, Duration::minutes(60));
            let id = r.approval_id();
            store.insert(r).expect("insert churn");
            store.remove(id).expect("remove churn");
        }

        // Thanks to auto-compaction, physical rows are FAR fewer than 201.
        let rows = physical_rows(tmp.path());
        assert!(
            rows < 50,
            "auto-compaction should keep the log small, got {rows} rows"
        );
        // The live keeper survived throughout.
        assert!(store.get(keeper_id).expect("get").is_some());
        assert_eq!(store.len().expect("len"), 1);
    }

    #[test]
    fn compact_on_empty_log_is_noop() {
        let tmp = TempPath::new("compact-empty");
        let store = JournalPendingStore::open(tmp.path()).expect("open");
        assert_eq!(store.compact().expect("compact"), 0);
        assert!(store.is_empty().expect("empty"));
        assert_eq!(physical_rows(tmp.path()), 0);
    }

    /// Regression test for closing the TOCTOU gap: compaction reads the
    /// state and rewrites the log **under the same file lock**
    /// ([`FileJournal::compact_with`]), so a concurrent insert cannot land
    /// in the gap and be lost. This test does not run real concurrency (the
    /// race is nondeterministic) — instead it proves the observable
    /// invariant that follows from the structure: an insert made AFTER
    /// compaction RETURNS lands AFTER the compacted live rows, and a reload
    /// produces exactly the correct state (both the compacted live rows AND
    /// the one inserted after compaction). Concurrent-append safety now
    /// follows from holding a single lock, not from lucky timing.
    #[test]
    fn compact_then_append_does_not_lose_post_compact_insert() {
        let tmp = TempPath::new("compact-toctou");
        let now = at(1_700_000_000);
        let store = JournalPendingStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        // Six inserts, remove three → dead rows accumulate.
        let mut ids = Vec::new();
        for _ in 0..6 {
            let record = record_at(now, Duration::minutes(60));
            ids.push(record.approval_id());
            store.insert(record).expect("insert");
        }
        for id in ids.iter().take(3) {
            store.remove(*id).expect("remove");
        }
        assert_eq!(store.len().expect("len"), 3, "3 live before compact");

        // Compact (atomic, under a single lock).
        let dropped = store.compact().expect("compact");
        assert_eq!(
            dropped, 6,
            "9 rows (6 put + 3 tombstone) → 3 live = 6 dropped"
        );
        assert_eq!(physical_rows(tmp.path()), 3, "only live rows after compact");

        // A record inserted AFTER compaction lands AFTER the live ones (not lost).
        let post = record_at(now, Duration::minutes(60));
        let post_id = post.approval_id();
        store.insert(post).expect("insert after compact");
        assert_eq!(store.len().expect("len"), 4, "3 live + 1 post-compact");
        assert!(
            store.get(post_id).expect("get").is_some(),
            "post-compact insert present"
        );

        // A reload produces EXACTLY the correct state: the compacted live rows +
        // the one inserted after compaction; removed ones stay removed.
        let resumed = JournalPendingStore::open(tmp.path()).expect("reopen");
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

    // ---- Rate limiter ----

    #[test]
    fn rate_limiter_allows_up_to_cap_then_denies() {
        let limiter = DangerousToolRateLimiter::new(60, 2);
        let now = at(1_700_000_000);

        limiter.check_and_record("being-a", now).expect("first");
        limiter.check_and_record("being-a", now).expect("second");
        // The third in the window → blocked.
        let err = limiter
            .check_and_record("being-a", now)
            .expect_err("third denied");
        assert!(matches!(err, ActionError::PolicyDenied(_)));
        assert_eq!(limiter.count_in_window("being-a", now), 2);
    }

    #[test]
    fn rate_limiter_is_per_being() {
        let limiter = DangerousToolRateLimiter::new(60, 1);
        let now = at(1_700_000_000);
        limiter
            .check_and_record("being-a", now)
            .expect("being-a first");
        // A different being → its own quota.
        limiter
            .check_and_record("being-b", now)
            .expect("being-b first");
        // being-a's quota is already exhausted.
        assert!(limiter.check_and_record("being-a", now).is_err());
    }

    #[test]
    fn rate_limiter_window_slides() {
        let limiter = DangerousToolRateLimiter::new(60, 1);
        let now = at(1_700_000_000);
        limiter.check_and_record("being-a", now).expect("first");
        assert!(limiter.check_and_record("being-a", now).is_err());
        // After the window (now + 61s) the old record is evicted → room again.
        limiter
            .check_and_record("being-a", at(1_700_000_061))
            .expect("after window slides");
    }
}
