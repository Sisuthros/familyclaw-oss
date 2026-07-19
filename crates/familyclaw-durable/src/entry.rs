//! Journal rows: [`JournalEntry`], [`StepId`], and [`EntryKind`].
//!
//! The journal is the durable substrate's **sole source of truth**. Every
//! row records one deterministically replayable event: a step completing, a
//! step failing, or a snapshot. Replay reads the rows in order and returns
//! the cached results without re-running side effects.

use serde::{Deserialize, Serialize};

use familyclaw_core::time::{self, Timestamp};

/// Identifier for a step's sequence position in the journal.
///
/// A simple 0-based monotonic index. Determinism requires that the same
/// code produce steps in the same order — `StepId` encodes this order, so
/// replay can compare the expected and found step position by position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StepId(u64);

impl StepId {
    /// The first step (index 0).
    pub const ZERO: StepId = StepId(0);

    /// Builds a step identifier from a raw index.
    #[must_use]
    pub const fn new(index: u64) -> Self {
        Self(index)
    }

    /// Returns the contained index.
    #[must_use]
    pub const fn index(self) -> u64 {
        self.0
    }

    /// Returns the identifier for the next step.
    ///
    /// Saturates at [`u64::MAX`] instead of overflowing — a durable log
    /// practically never reaches this, but saturation keeps the function
    /// panic-free.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl std::fmt::Display for StepId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// The kind of a journal row and its associated payload.
///
/// `#[non_exhaustive]` so new row kinds (e.g. timer, side-effect marker) can
/// be added later without breaking readers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EntryKind {
    /// The step ran successfully and its result has been recorded.
    ///
    /// During replay, the closure corresponding to this row is **not
    /// re-run** — `output` is returned directly, so side effects do not recur.
    StepCompleted {
        /// The step's logical name (the same on every run, deterministic).
        name: String,
        /// The step's returned result as a JSON value.
        output: serde_json::Value,
    },

    /// The step failed. The error is recorded so replay can return the same
    /// error without re-running the closure's logic.
    StepFailed {
        /// The step's logical name.
        name: String,
        /// The recorded error message.
        error: String,
    },

    /// A state snapshot. Condenses earlier rows into a single point from
    /// which replay can start quickly.
    Snapshot {
        /// The application state contained in the snapshot, as a JSON value.
        state: serde_json::Value,
    },

    /// An additional annotation in the log that **is not a workflow step**.
    ///
    /// Markers carry side information in the same append-only log (design
    /// §1: *"durable carries everything"*) without consuming any part of
    /// [`DurableContext`](crate::DurableContext) replay's step cursor.
    /// Example: dreaming-phase contradiction annotations
    /// (`familyclaw-dream`). [`DurableContext::new`](crate::DurableContext::new)
    /// filters markers out exactly like snapshots, so they cannot be
    /// confused with workflow steps ([`DurableError::NondeterministicReplay`]).
    ///
    /// [`DurableError::NondeterministicReplay`]: crate::DurableError::NondeterministicReplay
    Marker {
        /// The marker's logical name (e.g. "`memory_contradicted`").
        name: String,
        /// A free-form JSON payload.
        payload: serde_json::Value,
    },

    /// Session state persistence — a marker record that stores the
    /// message's origin (`MessageOrigin`) so the session can be restored
    /// during replay/startup.
    SessionState {
        /// The channel instance identifier.
        channel_id: String,
        /// The conversation/group identifier.
        conversation: String,
        /// The channel-specific sender identifier (for auditing).
        sender: String,
    },
}

impl EntryKind {
    /// Returns the step name if the row is tied to a named **workflow step**.
    ///
    /// Markers ([`EntryKind::Marker`]) carry their own name but **are not**
    /// steps, so they return `None` — this way they cannot accidentally hit
    /// replay's step-name comparison.
    #[must_use]
    pub fn step_name(&self) -> Option<&str> {
        match self {
            EntryKind::StepCompleted { name, .. } | EntryKind::StepFailed { name, .. } => {
                Some(name.as_str())
            }
            EntryKind::Snapshot { .. }
            | EntryKind::Marker { .. }
            | EntryKind::SessionState { .. } => None,
        }
    }

    /// Whether the row is a snapshot.
    #[must_use]
    pub const fn is_snapshot(&self) -> bool {
        matches!(self, EntryKind::Snapshot { .. })
    }

    /// Whether the row is a marker (an annotation outside the workflow step sequence).
    ///
    /// `SessionState` is stored under the marker category so it is filtered
    /// out of the replay cursor, while still being preserved in the journal
    /// for startup to read.
    #[must_use]
    pub const fn is_marker(&self) -> bool {
        matches!(
            self,
            EntryKind::Marker { .. } | EntryKind::SessionState { .. }
        )
    }

    /// Whether the row is a **workflow step** (completed or failed).
    ///
    /// `true` only for [`StepCompleted`](EntryKind::StepCompleted) /
    /// [`StepFailed`](EntryKind::StepFailed) rows. Snapshots and markers are
    /// NOT steps, so they are filtered out of the replay cursor.
    #[must_use]
    pub const fn is_step(&self) -> bool {
        matches!(
            self,
            EntryKind::StepCompleted { .. } | EntryKind::StepFailed { .. }
        )
    }
}

/// A single row of the durable journal.
///
/// Rows are append-only: once a row is written, it is never modified. This
/// is the foundation of the whole model — history is immutable, so replay
/// is deterministic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// The step's sequence position in the journal.
    pub step_id: StepId,
    /// The row's write timestamp (UTC) — for auditing/diagnostics only,
    /// does NOT affect replay determinism.
    pub timestamp: Timestamp,
    /// The row's kind and payload.
    pub kind: EntryKind,
}

impl JournalEntry {
    /// Builds a row with the given sequence position and kind, stamping it
    /// with the current time.
    #[must_use]
    pub fn new(step_id: StepId, kind: EntryKind) -> Self {
        Self {
            step_id,
            timestamp: time::now(),
            kind,
        }
    }

    /// Builds a row for a successful step.
    #[must_use]
    pub fn completed(step_id: StepId, name: impl Into<String>, output: serde_json::Value) -> Self {
        Self::new(
            step_id,
            EntryKind::StepCompleted {
                name: name.into(),
                output,
            },
        )
    }

    /// Builds a row for a failed step.
    #[must_use]
    pub fn failed(step_id: StepId, name: impl Into<String>, error: impl Into<String>) -> Self {
        Self::new(
            step_id,
            EntryKind::StepFailed {
                name: name.into(),
                error: error.into(),
            },
        )
    }

    /// Builds a snapshot row.
    #[must_use]
    pub fn snapshot(step_id: StepId, state: serde_json::Value) -> Self {
        Self::new(step_id, EntryKind::Snapshot { state })
    }

    /// Builds a marker row (an annotation outside the workflow step sequence).
    ///
    /// Markers carry side information in the same log as workflows, but
    /// [`DurableContext`](crate::DurableContext) filters them out of the
    /// replay cursor just like snapshots.
    #[must_use]
    pub fn marker(step_id: StepId, name: impl Into<String>, payload: serde_json::Value) -> Self {
        Self::new(
            step_id,
            EntryKind::Marker {
                name: name.into(),
                payload,
            },
        )
    }

    /// Builds a session-state persistence row.
    ///
    /// `SessionState` is a marker record (not a workflow step) so it does not
    /// interfere with the replay cursor. Stores the `MessageOrigin` data so the
    /// session can be restored at startup or during replay.
    #[must_use]
    pub fn session_state_entry(
        step_id: StepId,
        channel_id: impl Into<String>,
        conversation: impl Into<String>,
        sender: impl Into<String>,
    ) -> Self {
        Self::new(
            step_id,
            EntryKind::SessionState {
                channel_id: channel_id.into(),
                conversation: conversation.into(),
                sender: sender.into(),
            },
        )
    }

    /// Returns the row's step name if it has one.
    #[must_use]
    pub fn step_name(&self) -> Option<&str> {
        self.kind.step_name()
    }

    /// Retrieves the `SessionState` if the row's kind is `SessionState`.
    #[must_use]
    pub fn session_state(&self) -> Option<(&str, &str, &str)> {
        match &self.kind {
            EntryKind::SessionState {
                channel_id,
                conversation,
                sender,
            } => Some((channel_id, conversation, sender)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn step_id_next_and_display() {
        let zero = StepId::ZERO;
        assert_eq!(zero.index(), 0);
        assert_eq!(zero.next().index(), 1);
        assert_eq!(StepId::new(5).to_string(), "#5");
    }

    #[test]
    fn step_id_next_saturates_at_max() {
        let max = StepId::new(u64::MAX);
        assert_eq!(max.next().index(), u64::MAX);
    }

    #[test]
    fn entry_kind_step_name_and_snapshot() {
        let done = EntryKind::StepCompleted {
            name: "a".to_string(),
            output: json!(1),
        };
        assert_eq!(done.step_name(), Some("a"));
        assert!(!done.is_snapshot());
        assert!(done.is_step());
        assert!(!done.is_marker());

        let snap = EntryKind::Snapshot { state: json!({}) };
        assert_eq!(snap.step_name(), None);
        assert!(snap.is_snapshot());
        assert!(!snap.is_step());
        assert!(!snap.is_marker());
    }

    #[test]
    fn marker_is_not_a_step_and_has_no_step_name() {
        let marker = JournalEntry::marker(StepId::new(3), "memory_contradicted", json!({"x": 1}));
        // The marker's name does NOT show up as `step_name` — this way it does not hit replay's comparison.
        assert_eq!(marker.step_name(), None);
        assert!(marker.kind.is_marker());
        assert!(!marker.kind.is_step());
        assert!(!marker.kind.is_snapshot());
        match marker.kind {
            EntryKind::Marker { name, payload } => {
                assert_eq!(name, "memory_contradicted");
                assert_eq!(payload, json!({"x": 1}));
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[test]
    fn marker_entry_serde_roundtrip_with_snake_case_tag() {
        let entry = JournalEntry::marker(StepId::ZERO, "m", json!({"k": "v"}));
        let text = serde_json::to_string(&entry).expect("serialize");
        assert!(text.contains("\"kind\":\"marker\""));
        let back: JournalEntry = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(entry, back);
    }

    #[test]
    fn completed_entry_carries_output() {
        let entry = JournalEntry::completed(StepId::ZERO, "fetch", json!({"n": 42}));
        assert_eq!(entry.step_id, StepId::ZERO);
        assert_eq!(entry.step_name(), Some("fetch"));
        match entry.kind {
            EntryKind::StepCompleted { output, .. } => assert_eq!(output, json!({"n": 42})),
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[test]
    fn failed_entry_carries_error() {
        let entry = JournalEntry::failed(StepId::new(3), "save", "timeout");
        assert_eq!(entry.step_name(), Some("save"));
        match entry.kind {
            EntryKind::StepFailed { error, .. } => assert_eq!(error, "timeout"),
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[test]
    fn snapshot_entry_has_no_step_name() {
        let entry = JournalEntry::snapshot(StepId::new(2), json!({"acc": 10}));
        assert_eq!(entry.step_name(), None);
        assert!(entry.kind.is_snapshot());
    }

    #[test]
    fn entry_serde_roundtrip() {
        let entry = JournalEntry::completed(StepId::new(1), "step", json!([1, 2, 3]));
        let text = serde_json::to_string(&entry).expect("serialize");
        let back: JournalEntry = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(entry, back);
    }

    #[test]
    fn entry_kind_tag_is_snake_case() {
        let entry = JournalEntry::completed(StepId::ZERO, "x", json!(null));
        let text = serde_json::to_string(&entry).expect("serialize");
        assert!(text.contains("\"kind\":\"step_completed\""));
    }
}

#[cfg(test)]
mod timestamp_tests {
    use super::*;
    use serde_json::json;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn timestamp_doesnt_affect_replay() {
        // Create two entries with the same step_id and kind but at different times
        let entry1 = JournalEntry::completed(StepId::ZERO, "test", json!({"value": 42}));

        // Wait to ensure different timestamp
        thread::sleep(Duration::from_millis(10));

        let entry2 = JournalEntry::completed(StepId::ZERO, "test", json!({"value": 42}));

        // Timestamps WILL be different
        assert_ne!(
            entry1.timestamp, entry2.timestamp,
            "timestamps should differ"
        );

        // But entries themselves are NOT equal (because PartialEq includes timestamp)
        assert_ne!(
            entry1, entry2,
            "entries with different timestamps are not equal"
        );

        // However, replay logic ONLY compares step_name and kind
        assert_eq!(entry1.step_name(), entry2.step_name());
        assert_eq!(entry1.kind, entry2.kind);
        assert_eq!(entry1.step_id, entry2.step_id);
    }
}
