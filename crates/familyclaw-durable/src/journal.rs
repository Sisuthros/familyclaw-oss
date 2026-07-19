//! The [`Journal`] trait and its associated types.
//!
//! A journal is an append-only event log. Implementations ([`crate::InMemoryJournal`],
//! [`crate::FileJournal`]) provide the same interface; [`crate::DurableContext`]
//! is built on top of either a trait object or a generic parameter, so the
//! backing format (memory vs. file) can be swapped without retesting the logic.
//!
//! ## Object Safety
//! All methods take `&self` (not `&mut self`) so the trait is `dyn`-compatible.
//! Internal mutable state lives in a `Mutex` in the implementations.

use crate::entry::{JournalEntry, StepId};
use crate::error::Result;

/// Append-only event log for durable execution.
///
/// # Invariants
/// - **Append-only:** [`append`](Journal::append) always adds to the end of
///   the log; existing rows are never modified. This guarantees replay
///   determinism.
/// - **Order is preserved:** [`replay_from`](Journal::replay_from) returns
///   rows in the same order they were appended.
/// - **Panic-free:** all failures are returned as a [`Result`].
pub trait Journal: Send + Sync {
    /// Appends a row to the end of the log and ensures it is durably
    /// stored (in the file implementation: flush + fsync before returning).
    ///
    /// # Errors
    /// [`crate::DurableError::Io`] or [`crate::DurableError::Serde`] if the
    /// backing storage fails.
    fn append(&self, entry: JournalEntry) -> Result<()>;

    /// Returns all rows starting from the given sequence position
    /// (inclusive of `from`).
    ///
    /// `StepId::ZERO` returns the entire log. This is used during replay to
    /// load previously recorded steps.
    ///
    /// # Errors
    /// [`crate::DurableError::Io`], [`crate::DurableError::Serde`], or
    /// [`crate::DurableError::CorruptEntry`] if the log cannot be
    /// read/parsed.
    fn replay_from(&self, from: StepId) -> Result<Vec<JournalEntry>>;

    /// Returns all rows in the log from start to end.
    ///
    /// The default implementation delegates to
    /// [`replay_from`](Journal::replay_from) with `StepId::ZERO`.
    ///
    /// # Errors
    /// Same as [`replay_from`](Journal::replay_from).
    fn replay_all(&self) -> Result<Vec<JournalEntry>> {
        self.replay_from(StepId::ZERO)
    }

    /// Writes a snapshot row that condenses the current state into a
    /// single point.
    ///
    /// A snapshot does not remove earlier rows — it is an additional row
    /// from which replay can quickly restore state without re-running all
    /// prior steps.
    ///
    /// # Errors
    /// Same as [`append`](Journal::append).
    fn snapshot(&self, step_id: StepId, state: serde_json::Value) -> Result<()> {
        self.append(JournalEntry::snapshot(step_id, state))
    }

    /// Returns the number of rows in the log.
    ///
    /// # Errors
    /// Same as [`replay_all`](Journal::replay_all).
    fn len(&self) -> Result<usize> {
        Ok(self.replay_all()?.len())
    }

    /// Whether the log is empty.
    ///
    /// # Errors
    /// Same as [`len`](Journal::len).
    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// Implement Journal for `Box<dyn Journal>` so trait objects can be used
/// directly as the journal type in `DurableContext`.
impl<J: Journal + ?Sized> Journal for Box<J> {
    fn append(&self, entry: JournalEntry) -> Result<()> {
        (**self).append(entry)
    }

    fn replay_from(&self, from: StepId) -> Result<Vec<JournalEntry>> {
        (**self).replay_from(from)
    }

    fn replay_all(&self) -> Result<Vec<JournalEntry>> {
        (**self).replay_all()
    }

    fn snapshot(&self, step_id: StepId, state: serde_json::Value) -> Result<()> {
        (**self).snapshot(step_id, state)
    }

    fn len(&self) -> Result<usize> {
        (**self).len()
    }

    fn is_empty(&self) -> Result<bool> {
        (**self).is_empty()
    }
}

/// Implement Journal for `Arc<dyn Journal>` so trait objects can be used
/// with shared ownership.
impl<J: Journal + ?Sized> Journal for std::sync::Arc<J> {
    fn append(&self, entry: JournalEntry) -> Result<()> {
        (**self).append(entry)
    }

    fn replay_from(&self, from: StepId) -> Result<Vec<JournalEntry>> {
        (**self).replay_from(from)
    }

    fn replay_all(&self) -> Result<Vec<JournalEntry>> {
        (**self).replay_all()
    }

    fn snapshot(&self, step_id: StepId, state: serde_json::Value) -> Result<()> {
        (**self).snapshot(step_id, state)
    }

    fn len(&self) -> Result<usize> {
        (**self).len()
    }

    fn is_empty(&self) -> Result<bool> {
        (**self).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::JournalEntry;
    use serde_json::json;
    use std::sync::Mutex;

    /// Minimal test implementation that proves the trait's default methods
    /// (`replay_all`, `snapshot`, `len`, `is_empty`) work correctly on top of
    /// just `append`/`replay_from`.
    #[derive(Default)]
    struct VecJournal {
        entries: Mutex<Vec<JournalEntry>>,
    }

    impl Journal for VecJournal {
        fn append(&self, entry: JournalEntry) -> Result<()> {
            self.entries.lock().unwrap().push(entry);
            Ok(())
        }

        fn replay_from(&self, from: StepId) -> Result<Vec<JournalEntry>> {
            Ok(self
                .entries
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.step_id >= from)
                .cloned()
                .collect())
        }
    }

    #[test]
    fn default_methods_build_on_append_and_replay() {
        let j = VecJournal::default();
        assert!(j.is_empty().expect("is_empty"));
        assert_eq!(j.len().expect("len"), 0);

        j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
            .expect("append");
        j.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append");

        assert!(!j.is_empty().expect("is_empty"));
        assert_eq!(j.len().expect("len"), 2);
        assert_eq!(j.replay_all().expect("all").len(), 2);
    }

    #[test]
    fn replay_from_respects_offset() {
        let j = VecJournal::default();
        for i in 0..3 {
            j.append(JournalEntry::completed(StepId::new(i), "s", json!(i)))
                .expect("append");
        }
        let tail = j.replay_from(StepId::new(1)).expect("replay_from");
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].step_id, StepId::new(1));
    }

    #[test]
    fn snapshot_default_appends_snapshot_row() {
        let j = VecJournal::default();
        j.snapshot(StepId::ZERO, json!({"acc": 5}))
            .expect("snapshot");
        let all = j.replay_all().expect("all");
        assert_eq!(all.len(), 1);
        assert!(all[0].kind.is_snapshot());
    }
}
