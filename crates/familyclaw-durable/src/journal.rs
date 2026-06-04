//! [`Journal`]-trait ja siihen liittyvät tyypit.
//!
//! Journal on append-only tapahtumaloki. Toteutukset ([`crate::InMemoryJournal`],
//! [`crate::FileJournal`]) tarjoavat saman rajapinnan; [`crate::DurableContext`]
//! rakentuu trait-objektin tai geneerisen parametrin päälle, joten taustamuoto
//! (muisti vs. tiedosto) on vaihdettavissa testaamatta logiikkaa uudelleen.

use crate::entry::{JournalEntry, StepId};
use crate::error::Result;

/// Append-only tapahtumaloki durable-suoritukselle.
///
/// # Invariantit
/// - **Append-only:** [`append`](Journal::append) lisää aina lokin loppuun;
///   olemassa olevia rivejä ei koskaan muuteta. Tämä takaa replay-determinismin.
/// - **Järjestys säilyy:** [`replay_from`](Journal::replay_from) palauttaa rivit
///   samassa järjestyksessä kuin ne lisättiin.
/// - **Paniikiton:** kaikki epäonnistumiset palautuvat [`Result`]:na.
pub trait Journal {
    /// Lisää rivin lokin loppuun ja varmistaa että se on kestävästi
    /// tallennettu (tiedostototeutuksessa: flush + fsync ennen paluuta).
    ///
    /// # Errors
    /// [`crate::DurableError::Io`] tai [`crate::DurableError::Serde`] jos
    /// taustatallennus epäonnistuu.
    fn append(&mut self, entry: JournalEntry) -> Result<()>;

    /// Palauttaa kaikki rivit annetusta sekvenssipaikasta alkaen (ml. `from`).
    ///
    /// `StepId::ZERO` palauttaa koko lokin. Tätä käytetään replayssä lataamaan
    /// aiemmin tallennetut askeleet.
    ///
    /// # Errors
    /// [`crate::DurableError::Io`], [`crate::DurableError::Serde`] tai
    /// [`crate::DurableError::CorruptEntry`] jos lokia ei voi lukea/jäsentää.
    fn replay_from(&self, from: StepId) -> Result<Vec<JournalEntry>>;

    /// Palauttaa lokin kaikki rivit alusta loppuun.
    ///
    /// Oletustoteutus delegoi [`replay_from`](Journal::replay_from):lle
    /// `StepId::ZERO`:lla.
    ///
    /// # Errors
    /// Sama kuin [`replay_from`](Journal::replay_from).
    fn replay_all(&self) -> Result<Vec<JournalEntry>> {
        self.replay_from(StepId::ZERO)
    }

    /// Kirjoittaa snapshot-rivin joka tiivistää nykytilan yhdeksi pisteeksi.
    ///
    /// Snapshot ei poista aiempia rivejä — se on lisärivi josta replay voi
    /// nopeasti palauttaa tilan ajamatta kaikkia aiempia askelia uudelleen.
    ///
    /// # Errors
    /// Sama kuin [`append`](Journal::append).
    fn snapshot(&mut self, step_id: StepId, state: serde_json::Value) -> Result<()> {
        self.append(JournalEntry::snapshot(step_id, state))
    }

    /// Palauttaa lokin rivien lukumäärän.
    ///
    /// # Errors
    /// Sama kuin [`replay_all`](Journal::replay_all).
    fn len(&self) -> Result<usize> {
        Ok(self.replay_all()?.len())
    }

    /// Onko loki tyhjä.
    ///
    /// # Errors
    /// Sama kuin [`len`](Journal::len).
    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::JournalEntry;
    use serde_json::json;

    /// Minimaalinen testitoteutus joka todistaa että trait-oletusmetodit
    /// (`replay_all`, `snapshot`, `len`, `is_empty`) toimivat pelkän
    /// `append`/`replay_from` päälle.
    #[derive(Default)]
    struct VecJournal {
        entries: Vec<JournalEntry>,
    }

    impl Journal for VecJournal {
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
    }

    #[test]
    fn default_methods_build_on_append_and_replay() {
        let mut j = VecJournal::default();
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
        let mut j = VecJournal::default();
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
        let mut j = VecJournal::default();
        j.snapshot(StepId::ZERO, json!({"acc": 5}))
            .expect("snapshot");
        let all = j.replay_all().expect("all");
        assert_eq!(all.len(), 1);
        assert!(all[0].kind.is_snapshot());
    }
}
