//! Token Coherence — **MESI**-koherenssitilakone jaetulle artefaktille.
//!
//! Tausta (design §2, Token Coherence): kun monta agenttia jakaa saman tilan
//! (esim. jaettu muisti-artefakti), broadcast jokaisesta muutoksesta on
//! tuhlaavaa. Klassinen suoritin-välimuistien **MESI**-protokolla (Modified,
//! Exclusive, Shared, Invalid) antaa tähän valmiin, todistetun mallin: kukin
//! agentti pitää omaa kopiotaan ja vaihtaa tilaa luku-/kirjoitus-/mitätöinti-
//! tapahtumissa, jolloin broadcastia tarvitaan vain todellisten muutosten
//! kohdalla (90–95 % token-säästö vs. naivi broadcast).
//!
//! Tämä on **puhdas kirjastotilakone** — ei verkkoa, ei actoreita, ei I/O:ta.
//! [`CoherenceTracker`] mallintaa **yhden agentin näkymän** yhteen jaettuun
//! artefaktiin. Verkkokerros (kuka kuuntelee ketä) rakennetaan tämän päälle
//! erikseen; tilakone on testattavissa täysin deterministisesti.
//!
//! ## MESI-invariantit
//! - **Modified (M):** Tällä agentilla on ainoa, *muokattu* kopio (likainen).
//!   Muiden kopiot ovat Invalid.
//! - **Exclusive (E):** Tällä agentilla on ainoa kopio, joka on *puhdas*
//!   (sama kuin "totuus"). Muiden kopiot ovat Invalid.
//! - **Shared (S):** Useammalla agentilla voi olla puhdas kopio yhtä aikaa.
//! - **Invalid (I):** Tällä agentilla ei ole kelvollista kopiota.
//!
//! Ydininvariantti: M ja E ovat **yksinomistajia** — korkeintaan yksi agentti
//! voi olla M- tai E-tilassa artefaktia kohden samanaikaisesti.
//!
//! ## OSS-raja (KERROS A)
//! Ei kovakoodattuja perheen nimiä, ID:itä eikä avaimia.

use serde::{Deserialize, Serialize};

/// MESI-koherenssitila yhden agentin näkymälle jaettuun artefaktiin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MesiState {
    /// Ainoa kopio, muokattu (likainen). Muiden kopiot ovat Invalid.
    Modified,
    /// Ainoa kopio, puhdas (sama kuin totuus). Muiden kopiot ovat Invalid.
    Exclusive,
    /// Jaettu, puhdas kopio — useammalla agentilla voi olla samaan aikaan.
    Shared,
    /// Ei kelvollista kopiota.
    Invalid,
}

impl MesiState {
    /// Lyhyt vakaa kirjaintunniste (`M`/`E`/`S`/`I`) lokitukseen ja metriikkaan.
    #[must_use]
    pub const fn as_char(&self) -> char {
        match self {
            MesiState::Modified => 'M',
            MesiState::Exclusive => 'E',
            MesiState::Shared => 'S',
            MesiState::Invalid => 'I',
        }
    }

    /// Onko tässä tilassa kelvollinen (luettavissa oleva) kopio?
    /// Tosi kaikille paitsi [`Invalid`](MesiState::Invalid).
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        !matches!(self, MesiState::Invalid)
    }

    /// Onko kopio likainen (muutoksia, jotka pitää kirjoittaa takaisin
    /// totuuteen ennen mitätöintiä)? Tosi vain [`Modified`](MesiState::Modified).
    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        matches!(self, MesiState::Modified)
    }

    /// Onko tämä **yksinomistaja**-tila (M tai E)? Korkeintaan yksi agentti
    /// voi olla tällaisessa tilassa artefaktia kohden.
    #[must_use]
    pub const fn is_exclusive_owner(&self) -> bool {
        matches!(self, MesiState::Modified | MesiState::Exclusive)
    }
}

/// Yhden agentin MESI-tilaseuranta yhteen jaettuun artefaktiin.
///
/// Siirtymät ([`local_read`](CoherenceTracker::local_read),
/// [`local_write`](CoherenceTracker::local_write),
/// [`remote_read`](CoherenceTracker::remote_read),
/// [`remote_write`](CoherenceTracker::remote_write),
/// [`invalidate`](CoherenceTracker::invalidate)) noudattavat MESI-sääntöjä.
/// Tracker alkaa [`Invalid`](MesiState::Invalid)-tilassa (agentilla ei vielä
/// kopiota).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoherenceTracker {
    state: MesiState,
}

impl CoherenceTracker {
    /// Luo trackerin alkutilassa [`Invalid`](MesiState::Invalid).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: MesiState::Invalid,
        }
    }

    /// Luo trackerin annetussa alkutilassa (esim. ladatun tilannekuvan
    /// palautukseen).
    #[must_use]
    pub const fn with_state(state: MesiState) -> Self {
        Self { state }
    }

    /// Nykyinen MESI-tila.
    #[must_use]
    pub const fn state(&self) -> MesiState {
        self.state
    }

    /// **Paikallinen luku** tältä agentilta.
    ///
    /// - **Invalid → Shared:** luku-miss; kopio haetaan ja jaetaan. (Yksin-
    ///   kertaistus: emme erottele E:tä S:stä lukumissin yhteydessä, koska se
    ///   vaatisi tiedon "olenko ainoa hakija". Konservatiivinen S on aina
    ///   turvallinen — kirjoitus nostaa M:ään.)
    /// - **Shared/Exclusive/Modified → ennallaan:** osuma; tila ei muutu.
    ///
    /// Palauttaa tilan luvun jälkeen.
    pub fn local_read(&mut self) -> MesiState {
        if self.state == MesiState::Invalid {
            self.state = MesiState::Shared;
        }
        self.state
    }

    /// **Paikallinen kirjoitus** tältä agentilta.
    ///
    /// Kirjoitus vaatii **yksinomistajuuden**: tila siirtyy aina
    /// [`Modified`](MesiState::Modified):iin. Kaikkien muiden agenttien
    /// kopiot on tämän seurauksena mitätöitävä (kutsuja vastaa broadcastista;
    /// muiden trackerien tulee saada [`remote_write`](Self::remote_write) tai
    /// [`invalidate`](Self::invalidate)).
    ///
    /// Sallittu kaikista lähtötiloista:
    /// - Invalid/Shared → Modified (vaatii muiden mitätöinnin)
    /// - Exclusive → Modified (ei tarvitse muiden mitätöintiä; oli jo ainoa)
    /// - Modified → Modified (ei muutosta)
    ///
    /// Palauttaa tilan kirjoituksen jälkeen ([`Modified`](MesiState::Modified)).
    pub fn local_write(&mut self) -> MesiState {
        self.state = MesiState::Modified;
        self.state
    }

    /// **Toisen agentin luku** samasta artefaktista (snoop: `BusRd`).
    ///
    /// Yksinomistajan on tiputtava jaetuksi, jotta lukija saa puhtaan kopion:
    /// - **Modified → Shared:** likainen data kirjoitetaan takaisin (write-back)
    ///   ja jaetaan. [`needs_writeback`](RemoteReadOutcome::needs_writeback)
    ///   on tosi.
    /// - **Exclusive → Shared:** jaetaan ilman write-backia.
    /// - **Shared → Shared:** ennallaan.
    /// - **Invalid → Invalid:** ei vaikutusta (meillä ei ollut kopiota).
    ///
    /// Palauttaa [`RemoteReadOutcome`]:n, joka kertoo uuden tilan ja
    /// tarvitseeko likainen data kirjoittaa takaisin.
    pub fn remote_read(&mut self) -> RemoteReadOutcome {
        let needs_writeback = self.state == MesiState::Modified;
        if self.state.is_valid() {
            self.state = MesiState::Shared;
        }
        RemoteReadOutcome {
            state: self.state,
            needs_writeback,
        }
    }

    /// **Toisen agentin kirjoitus** samaan artefaktiin (snoop: `BusRdX`).
    ///
    /// Tämän agentin kopio on aina mitätöitävä — toinen ottaa yksinomistajuuden:
    /// - **Modified → Invalid:** vaatii write-backin ennen mitätöintiä.
    ///   [`needs_writeback`](RemoteWriteOutcome::needs_writeback) on tosi.
    /// - **Exclusive/Shared → Invalid:** mitätöinti ilman write-backia.
    /// - **Invalid → Invalid:** ei vaikutusta.
    ///
    /// Palauttaa [`RemoteWriteOutcome`]:n.
    pub fn remote_write(&mut self) -> RemoteWriteOutcome {
        let needs_writeback = self.state == MesiState::Modified;
        self.state = MesiState::Invalid;
        RemoteWriteOutcome {
            state: self.state,
            needs_writeback,
        }
    }

    /// Pakottaa tämän agentin kopion [`Invalid`](MesiState::Invalid)-tilaan
    /// (esim. eksplisiittinen mitätöintikäsky). Tilakone-mielessä sama kuin
    /// [`remote_write`](Self::remote_write):n tilavaikutus, mutta ilman
    /// write-back-signaalia — käytä `remote_write`:ä jos likainen data pitää
    /// säilyttää.
    pub fn invalidate(&mut self) -> MesiState {
        self.state = MesiState::Invalid;
        self.state
    }
}

impl Default for CoherenceTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Toisen agentin luvun ([`CoherenceTracker::remote_read`]) lopputulos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteReadOutcome {
    /// Tämän agentin tila luvun jälkeen.
    pub state: MesiState,
    /// Pitikö likainen (Modified) data kirjoittaa takaisin totuuteen?
    pub needs_writeback: bool,
}

/// Toisen agentin kirjoituksen ([`CoherenceTracker::remote_write`]) lopputulos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteWriteOutcome {
    /// Tämän agentin tila kirjoituksen jälkeen (aina [`MesiState::Invalid`]).
    pub state: MesiState,
    /// Pitikö likainen (Modified) data kirjoittaa takaisin ennen mitätöintiä?
    pub needs_writeback: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_invalid() {
        let t = CoherenceTracker::new();
        assert_eq!(t.state(), MesiState::Invalid);
        assert!(!t.state().is_valid());
        assert_eq!(CoherenceTracker::default().state(), MesiState::Invalid);
    }

    #[test]
    fn local_read_miss_loads_shared_then_hit_is_stable() {
        let mut t = CoherenceTracker::new();
        // Invalid → Shared (luku-miss).
        assert_eq!(t.local_read(), MesiState::Shared);
        // Shared → Shared (osuma, ei muutosta).
        assert_eq!(t.local_read(), MesiState::Shared);
        assert!(t.state().is_valid());
        assert!(!t.state().is_dirty());
    }

    #[test]
    fn local_write_takes_modified_from_any_state() {
        for start in [
            MesiState::Invalid,
            MesiState::Shared,
            MesiState::Exclusive,
            MesiState::Modified,
        ] {
            let mut t = CoherenceTracker::with_state(start);
            assert_eq!(t.local_write(), MesiState::Modified);
            assert!(t.state().is_dirty());
            assert!(t.state().is_exclusive_owner());
        }
    }

    #[test]
    fn modified_plus_remote_read_becomes_shared_with_writeback() {
        // Ydin-MESI-sääntö: Modified + toisen read → Shared (write-back).
        let mut t = CoherenceTracker::with_state(MesiState::Modified);
        let out = t.remote_read();
        assert_eq!(out.state, MesiState::Shared);
        assert!(out.needs_writeback, "likainen data kirjoitetaan takaisin");
        assert_eq!(t.state(), MesiState::Shared);
    }

    #[test]
    fn exclusive_plus_remote_read_becomes_shared_no_writeback() {
        let mut t = CoherenceTracker::with_state(MesiState::Exclusive);
        let out = t.remote_read();
        assert_eq!(out.state, MesiState::Shared);
        assert!(!out.needs_writeback, "puhdasta ei tarvitse kirjoittaa");
    }

    #[test]
    fn remote_read_on_invalid_stays_invalid() {
        let mut t = CoherenceTracker::new();
        let out = t.remote_read();
        assert_eq!(out.state, MesiState::Invalid);
        assert!(!out.needs_writeback);
    }

    #[test]
    fn local_write_then_others_invalidated_via_remote_write() {
        // Ydin-MESI-sääntö: write → muut Invalid.
        // Agentti A kirjoittaa (M); agentti B:n näkymä saa remote_write → Invalid.
        let mut a = CoherenceTracker::with_state(MesiState::Shared);
        let mut b = CoherenceTracker::with_state(MesiState::Shared);

        assert_eq!(a.local_write(), MesiState::Modified);
        // B snooppaa A:n kirjoituksen → mitätöityy.
        let out = b.remote_write();
        assert_eq!(out.state, MesiState::Invalid);
        assert!(!out.needs_writeback, "B oli vain Shared, ei likainen");
        assert!(!b.state().is_valid());
    }

    #[test]
    fn remote_write_on_modified_requires_writeback() {
        let mut t = CoherenceTracker::with_state(MesiState::Modified);
        let out = t.remote_write();
        assert_eq!(out.state, MesiState::Invalid);
        assert!(
            out.needs_writeback,
            "likainen M kirjoitetaan takaisin ennen I"
        );
    }

    #[test]
    fn invalidate_forces_invalid_without_writeback_signal() {
        let mut t = CoherenceTracker::with_state(MesiState::Modified);
        assert_eq!(t.invalidate(), MesiState::Invalid);
        assert_eq!(t.state(), MesiState::Invalid);
    }

    #[test]
    fn exclusive_owner_invariant_only_m_and_e() {
        assert!(MesiState::Modified.is_exclusive_owner());
        assert!(MesiState::Exclusive.is_exclusive_owner());
        assert!(!MesiState::Shared.is_exclusive_owner());
        assert!(!MesiState::Invalid.is_exclusive_owner());
    }

    #[test]
    fn state_chars_are_stable() {
        assert_eq!(MesiState::Modified.as_char(), 'M');
        assert_eq!(MesiState::Exclusive.as_char(), 'E');
        assert_eq!(MesiState::Shared.as_char(), 'S');
        assert_eq!(MesiState::Invalid.as_char(), 'I');
    }

    #[test]
    fn mesi_state_serde_roundtrip() {
        for s in [
            MesiState::Modified,
            MesiState::Exclusive,
            MesiState::Shared,
            MesiState::Invalid,
        ] {
            let json = serde_json::to_string(&s).expect("serialize");
            let back: MesiState = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(s, back);
        }
    }

    #[test]
    fn full_coherence_scenario_two_agents() {
        // Realistinen MESI-tarina kahdelle agentille saman artefaktin ympärillä.
        let mut a = CoherenceTracker::new();
        let mut b = CoherenceTracker::new();

        // A lukee ensin: Invalid → Shared.
        assert_eq!(a.local_read(), MesiState::Shared);
        // A kirjoittaa: Shared → Modified (B pitäisi mitätöidä, mutta B oli I).
        assert_eq!(a.local_write(), MesiState::Modified);
        assert_eq!(b.state(), MesiState::Invalid);

        // B lukee → snooppaa A:n: A Modified → Shared (write-back), B → Shared.
        let a_out = a.remote_read();
        assert!(a_out.needs_writeback);
        assert_eq!(a.state(), MesiState::Shared);
        assert_eq!(b.local_read(), MesiState::Shared);

        // B kirjoittaa: B → Modified, A snooppaa → Invalid.
        assert_eq!(b.local_write(), MesiState::Modified);
        let a_out = a.remote_write();
        assert_eq!(a.state(), MesiState::Invalid);
        assert!(
            !a_out.needs_writeback,
            "A oli Shared (puhdas), ei write-backia"
        );
    }
}
