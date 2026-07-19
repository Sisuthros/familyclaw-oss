//! [`InMemoryJournal`] — a non-durable journal implementation for testing
//! and development.
//!
//! Holds rows in a `Vec` guarded by `Arc<Mutex<...>>` so the trait is
//! `dyn`-compatible. Does not survive a process restart — use
//! [`crate::FileJournal`] for crash resistance. This is still useful for
//! unit tests and for verifying deterministic replay without touching disk.

use crate::entry::{JournalEntry, StepId};
use crate::error::Result;
use crate::journal::Journal;
use std::sync::{Arc, Mutex};

/// An in-memory append-only journal.
///
/// Preserves rows in append order. Cloning is deep (a full copy of the
/// rows), so modifying a cloned journal does not affect the original.
#[derive(Debug, Default)]
pub struct InMemoryJournal {
    entries: Arc<Mutex<Vec<JournalEntry>>>,
}

impl Clone for InMemoryJournal {
    fn clone(&self) -> Self {
        let entries = self.entries.lock().unwrap().clone();
        Self {
            entries: Arc::new(Mutex::new(entries)),
        }
    }
}

impl InMemoryJournal {
    /// Creates an empty in-memory journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an in-memory journal from ready-made rows (e.g. loaded from a
    /// prior log).
    #[must_use]
    pub fn from_entries(entries: Vec<JournalEntry>) -> Self {
        Self {
            entries: Arc::new(Mutex::new(entries)),
        }
    }

    /// Returns all rows without cloning the wrapper.
    #[must_use]
    pub fn entries(&self) -> Vec<JournalEntry> {
        self.entries.lock().unwrap().clone()
    }

    /// Consumes the journal and returns its rows.
    #[must_use]
    pub fn into_entries(self) -> Vec<JournalEntry> {
        Arc::try_unwrap(self.entries).unwrap().into_inner().unwrap()
    }
}

impl Journal for InMemoryJournal {
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

    fn replay_all(&self) -> Result<Vec<JournalEntry>> {
        Ok(self.entries.lock().unwrap().clone())
    }

    fn len(&self) -> Result<usize> {
        Ok(self.entries.lock().unwrap().len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_journal_is_empty() {
        let j = InMemoryJournal::new();
        assert!(j.is_empty().expect("is_empty"));
        assert!(j.entries().is_empty());
    }

    #[test]
    fn append_then_replay_preserves_order() {
        let j = InMemoryJournal::new();
        for i in 0..4 {
            j.append(JournalEntry::completed(StepId::new(i), "s", json!(i)))
                .expect("append");
        }
        let all = j.replay_all().expect("all");
        assert_eq!(all.len(), 4);
        for (i, e) in all.iter().enumerate() {
            assert_eq!(e.step_id, StepId::new(i as u64));
        }
    }

    #[test]
    fn from_entries_and_into_entries_roundtrip() {
        let entries = vec![
            JournalEntry::completed(StepId::ZERO, "a", json!(1)),
            JournalEntry::failed(StepId::new(1), "b", "err"),
        ];
        let j = InMemoryJournal::from_entries(entries.clone());
        assert_eq!(j.into_entries(), entries);
    }

    #[test]
    fn replay_from_filters_by_step_id() {
        let j = InMemoryJournal::new();
        j.append(JournalEntry::completed(StepId::new(0), "a", json!(0)))
            .expect("append");
        j.append(JournalEntry::completed(StepId::new(1), "b", json!(1)))
            .expect("append");
        j.append(JournalEntry::completed(StepId::new(2), "c", json!(2)))
            .expect("append");

        let tail = j.replay_from(StepId::new(2)).expect("replay_from");
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].step_name(), Some("c"));
    }

    #[test]
    fn clone_is_independent() {
        let original = InMemoryJournal::new();
        original
            .append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
            .expect("append");
        let copy = original.clone();
        copy.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append");
        assert_eq!(original.len().expect("len"), 1);
        assert_eq!(copy.len().expect("len"), 2);
    }
}
