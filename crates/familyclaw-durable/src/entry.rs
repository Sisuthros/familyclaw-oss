//! Journal-rivit: [`JournalEntry`], [`StepId`] ja [`EntryKind`].
//!
//! Journal on durable-substraatin **ainoa totuuden lähde**. Jokainen rivi
//! tallentaa yhden deterministisesti toistettavan tapahtuman: askeleen
//! valmistumisen, askeleen epäonnistumisen tai snapshotin. Replay lukee
//! rivit järjestyksessä ja palauttaa cachetut tulokset ajamatta sivuvaikutuksia
//! uudelleen.

use serde::{Deserialize, Serialize};

use familyclaw_core::time::{self, Timestamp};

/// Askeleen sekvenssipaikan tunniste journalissa.
///
/// Yksinkertainen 0-pohjainen monotoninen indeksi. Determinismi vaatii että
/// sama koodi tuottaa askeleet samassa järjestyksessä — `StepId` koodaa tämän
/// järjestyksen, joten replay voi verrata odotettua ja löydettyä askelta
/// paikka paikalta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StepId(u64);

impl StepId {
    /// Ensimmäinen askel (indeksi 0).
    pub const ZERO: StepId = StepId(0);

    /// Rakentaa askel-tunnisteen raa'asta indeksistä.
    #[must_use]
    pub const fn new(index: u64) -> Self {
        Self(index)
    }

    /// Palauttaa sisällä olevan indeksin.
    #[must_use]
    pub const fn index(self) -> u64 {
        self.0
    }

    /// Palauttaa seuraavan askeleen tunnisteen.
    ///
    /// Saturoituu [`u64::MAX`]:iin ylivuodon sijaan — durable-loki ei käytännössä
    /// koskaan saavuta tätä, mutta saturointi pitää funktion paniikittomana.
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

/// Journal-rivin laji ja siihen liittyvä hyötykuorma.
///
/// `#[non_exhaustive]` jotta uusia rivilajeja (esim. timer, side-effect-marker)
/// voi lisätä myöhemmin rikkomatta lukijoita.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EntryKind {
    /// Askel suoritettiin onnistuneesti ja sen tulos on tallennettu.
    ///
    /// Replayssä tätä riviä vastaava suljin **ei aja uudelleen** — `output`
    /// palautetaan suoraan, joten sivuvaikutukset eivät toistu.
    StepCompleted {
        /// Askeleen looginen nimi (sama joka ajolla, deterministinen).
        name: String,
        /// Askeleen palauttama tulos JSON-arvona.
        output: serde_json::Value,
    },

    /// Askel epäonnistui. Virhe tallennetaan jotta replay voi palauttaa
    /// saman virheen ajamatta sulkimellista logiikkaa uudelleen.
    StepFailed {
        /// Askeleen looginen nimi.
        name: String,
        /// Tallennettu virheviesti.
        error: String,
    },

    /// Tilan tilannekuva (snapshot). Tiivistää aiemmat rivit yhdeksi
    /// pisteeksi josta replay voi alkaa nopeasti.
    Snapshot {
        /// Snapshotin sisältämä sovellustila JSON-arvona.
        state: serde_json::Value,
    },

    /// Lisäannotaatio lokiin, joka **ei ole workflow-askel**.
    ///
    /// Markerit kantavat sivutietoa samassa append-only-lokissa (design §1:
    /// *"durable carries everything"*) ajamatta osaa
    /// [`DurableContext`](crate::DurableContext)-replayn askelkursorista.
    /// Esimerkki: dreaming-vaiheen ristiriitamerkinnät
    /// (`familyclaw-dream`). [`DurableContext::new`](crate::DurableContext::new)
    /// suodattaa markerit pois täsmälleen kuten snapshotit, joten ne eivät voi
    /// sekoittua workflow-askeliin ([`DurableError::NondeterministicReplay`]).
    ///
    /// [`DurableError::NondeterministicReplay`]: crate::DurableError::NondeterministicReplay
    Marker {
        /// Markerin looginen nimi (esim. `"memory_contradicted"`).
        name: String,
        /// Vapaamuotoinen JSON-hyötykuorma.
        payload: serde_json::Value,
    },
}

impl EntryKind {
    /// Palauttaa askeleen nimen jos rivi liittyy nimettyyn **workflow-askeleeseen**.
    ///
    /// Markerit ([`EntryKind::Marker`]) kantavat oman nimensä mutta **eivät**
    /// ole askelia, joten ne palauttavat `None` — näin ne eivät voi vahingossa
    /// osua replayn askel-nimivertailuun.
    #[must_use]
    pub fn step_name(&self) -> Option<&str> {
        match self {
            EntryKind::StepCompleted { name, .. } | EntryKind::StepFailed { name, .. } => {
                Some(name.as_str())
            }
            EntryKind::Snapshot { .. } | EntryKind::Marker { .. } => None,
        }
    }

    /// Onko rivi snapshot.
    #[must_use]
    pub const fn is_snapshot(&self) -> bool {
        matches!(self, EntryKind::Snapshot { .. })
    }

    /// Onko rivi marker (workflow-askeleen ulkopuolinen annotaatio).
    #[must_use]
    pub const fn is_marker(&self) -> bool {
        matches!(self, EntryKind::Marker { .. })
    }

    /// Onko rivi **workflow-askel** (valmistunut tai epäonnistunut).
    ///
    /// `true` vain [`StepCompleted`](EntryKind::StepCompleted) /
    /// [`StepFailed`](EntryKind::StepFailed)-riveille. Snapshotit ja markerit
    /// EIVÄT ole askelia, joten ne suodatetaan replay-kursorista pois.
    #[must_use]
    pub const fn is_step(&self) -> bool {
        matches!(
            self,
            EntryKind::StepCompleted { .. } | EntryKind::StepFailed { .. }
        )
    }
}

/// Yksi durable-journalin rivi.
///
/// Rivit ovat append-only: kun rivi on kirjoitettu, sitä ei koskaan muuteta.
/// Tämä on koko mallin perusta — historia on muuttumaton, joten replay on
/// deterministinen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Askeleen sekvenssipaikka journalissa.
    pub step_id: StepId,
    /// Rivin kirjoitushetki (UTC) — vain auditointia/diagnostiikkaa varten,
    /// EI vaikuta replay-determinismiin.
    pub timestamp: Timestamp,
    /// Rivin laji ja hyötykuorma.
    pub kind: EntryKind,
}

impl JournalEntry {
    /// Rakentaa rivin annetulla sekvenssipaikalla ja lajilla, leimaten sen
    /// nykyhetkellä.
    #[must_use]
    pub fn new(step_id: StepId, kind: EntryKind) -> Self {
        Self {
            step_id,
            timestamp: time::now(),
            kind,
        }
    }

    /// Rakentaa onnistuneen askeleen rivin.
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

    /// Rakentaa epäonnistuneen askeleen rivin.
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

    /// Rakentaa snapshot-rivin.
    #[must_use]
    pub fn snapshot(step_id: StepId, state: serde_json::Value) -> Self {
        Self::new(step_id, EntryKind::Snapshot { state })
    }

    /// Rakentaa marker-rivin (workflow-askeleen ulkopuolinen annotaatio).
    ///
    /// Markerit kantavat sivutietoa samassa lokissa kuin workflowit, mutta
    /// [`DurableContext`](crate::DurableContext) suodattaa ne replay-kursorista
    /// pois kuten snapshotit.
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

    /// Palauttaa rivin askeleen nimen jos sellainen on.
    #[must_use]
    pub fn step_name(&self) -> Option<&str> {
        self.kind.step_name()
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
        // Markerin nimi EI näy `step_name`:nä — näin se ei osu replay-vertailuun.
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
