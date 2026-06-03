//! [`InMemoryJournal`] — kestämätön journal-toteutus testaukseen ja
//! kehitykseen.
//!
//! Pitää rivit `Vec`-vektorissa. Ei kestä prosessin uudelleenkäynnistystä —
//! kaatumiskestävyyteen käytä [`crate::FileJournal`]:ia. Tämä on silti hyödyllinen
//! yksikkötesteissä ja deterministisen replayn varmentamisessa ilman levyä.

use crate::entry::{JournalEntry, StepId};
use crate::error::Result;
use crate::journal::Journal;

/// Muistinvarainen append-only journal.
///
/// Säilyttää rivit lisäysjärjestyksessä. Klooni on syvä (täysi kopio rivistä),
/// joten kloonatun journalin muokkaus ei vaikuta alkuperäiseen.
#[derive(Debug, Clone, Default)]
pub struct InMemoryJournal {
    entries: Vec<JournalEntry>,
}

impl InMemoryJournal {
    /// Luo tyhjän muistijournalin.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Luo muistijournalin valmiista riveistä (esim. aiemmasta lokista
    /// ladatuista).
    #[must_use]
    pub fn from_entries(entries: Vec<JournalEntry>) -> Self {
        Self { entries }
    }

    /// Palauttaa viittauksen kaikkiin riveihin ilman kloonausta.
    #[must_use]
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// Kuluttaa journalin ja palauttaa rivit.
    #[must_use]
    pub fn into_entries(self) -> Vec<JournalEntry> {
        self.entries
    }
}

impl Journal for InMemoryJournal {
    fn append(&mut self, entry: JournalEntry) -> Result<()> {
        self.entries.push(entry);
        Ok(())
    }

    fn replay_from(&self, from: StepId) -> Result<Vec<JournalEntry>> {
        Ok(self
            .entries
            .iter()
            .filter(|e| e.step_id >= from)
            .cloned()
            .collect())
    }

    fn replay_all(&self) -> Result<Vec<JournalEntry>> {
        Ok(self.entries.clone())
    }

    fn len(&self) -> Result<usize> {
        Ok(self.entries.len())
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
        let mut j = InMemoryJournal::new();
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
        let mut j = InMemoryJournal::new();
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
        let mut original = InMemoryJournal::new();
        original
            .append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
            .expect("append");
        let mut copy = original.clone();
        copy.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append");
        assert_eq!(original.len().expect("len"), 1);
        assert_eq!(copy.len().expect("len"), 2);
    }
}
