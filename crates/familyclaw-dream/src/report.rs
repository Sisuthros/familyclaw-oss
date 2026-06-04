//! Unijakson tulosraportti: [`DreamReport`] ja [`Reflection`].
//!
//! Unijakso ([`crate::DreamCycle`]) tuottaa raportin joka kertoo *mitä yön
//! aikana tapahtui*: kuinka monta muistoa yhdistettiin, pudotettiin tai
//! arkistoitiin, ja minkä reflektiot konsolidaatio synnytti. Raportti on
//! puhdasta dataa — se sarjallistuu lokeihin ja peilaa Amplifier-proteesin
//! "freshness audit" -palautteen natiiviksi (design §2.3).

use serde::{Deserialize, Serialize};

use familyclaw_core::{MessageId, Timestamp};

/// Yksittäinen unireflektio — koneluettava merkintä siitä mitä jokin
/// konsolidaatiovaihe teki yhdelle muistolle.
///
/// Reflektiot eivät ole vapaata proosaa vaan jäsenneltyjä tapahtumia, jotta
/// ne voidaan auditoida ja toistaa deterministisesti. `note`-kenttä on
/// ihmisluettava tiivistys, mutta `kind` + `memory` ovat kone-luettava
/// totuus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reflection {
    /// Mihin konsolidaatiovaiheeseen reflektio liittyy.
    pub kind: ReflectionKind,
    /// Muisto jota reflektio koskee (kohde tai säilytetty edustaja).
    pub memory: MessageId,
    /// Ihmisluettava tiivistys (esim. `"merged 3 near-duplicates"`).
    pub note: String,
}

impl Reflection {
    /// Rakentaa reflektion annetulle vaiheelle, muistolle ja tiivistykselle.
    #[must_use]
    pub fn new(kind: ReflectionKind, memory: MessageId, note: impl Into<String>) -> Self {
        Self {
            kind,
            memory,
            note: note.into(),
        }
    }
}

/// Mikä konsolidaatiovaihe reflektion synnytti.
///
/// `#[non_exhaustive]` jotta uusia vaiheita (esim. tulevan latent-pohjaisen
/// klusteroinnin) voi lisätä rikkomatta lukijoita.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReflectionKind {
    /// Lähes-identtisiä muistoja yhdistettiin yhdeksi edustajaksi.
    Merged,
    /// Vanhentunut/ristiriitainen muisto pudotettiin (tombstoned).
    Dropped,
    /// Suhteellinen päiväys ("eilen") muutettiin absoluuttiseksi.
    DateAbsolutized,
    /// Tärkeä muisto vahvistettiin (säilyvyyttä kasvatettiin).
    Strengthened,
    /// Matala-retention muisto arkistoitiin.
    Archived,
}

impl ReflectionKind {
    /// Vakaa, kone-luettava nimi (`snake_case`) — sama kuin serde-esitys.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ReflectionKind::Merged => "merged",
            ReflectionKind::Dropped => "dropped",
            ReflectionKind::DateAbsolutized => "date_absolutized",
            ReflectionKind::Strengthened => "strengthened",
            ReflectionKind::Archived => "archived",
        }
    }
}

impl std::fmt::Display for ReflectionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Yhden unijakson koottu tulos.
///
/// Laskurit kertovat *kuinka monta* muistoa kukin vaihe käsitteli;
/// [`reflections`](DreamReport::reflections) sisältää tapahtumakohtaiset
/// merkinnät. Raportti rakennetaan vaiheittain [`DreamReport::default`]:sta
/// tai [`DreamReport::new`]:llä ja kerätään yhteen unijakson lopuksi.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamReport {
    /// Hetki jolloin unijakso ajettiin (UTC). `None` kunnes asetettu.
    #[serde(default)]
    pub ran_at: Option<Timestamp>,
    /// Yhdistettyjen (poistettujen) duplikaattien määrä — EI mukaan luettuna
    /// säilytettyjä edustajia.
    pub merged: usize,
    /// Pudotettujen (tombstoned) vanhentuneiden/ristiriitaisten määrä.
    pub dropped: usize,
    /// Arkistoitujen matala-retention muistojen määrä.
    pub archived: usize,
    /// Vahvistettujen tärkeiden muistojen määrä.
    pub strengthened: usize,
    /// Absolutisoitujen päiväysten määrä.
    pub dates_absolutized: usize,
    /// Läpikäytyjen muistojen kokonaismäärä unijakson alussa.
    pub scanned: usize,
    /// Tapahtumakohtaiset reflektiot, syntyjärjestyksessä.
    #[serde(default)]
    pub reflections: Vec<Reflection>,
}

impl DreamReport {
    /// Luo tyhjän raportin annetulla ajohetkellä.
    #[must_use]
    pub fn new(ran_at: Timestamp) -> Self {
        Self {
            ran_at: Some(ran_at),
            ..Self::default()
        }
    }

    /// Lisää reflektion ja kasvattaa vastaavan laskurin.
    ///
    /// Tämä on raportin ainoa mutaatioreitti, joten laskurit ja reflektiot
    /// pysyvät aina synkassa (yksi reflektio ⇒ yksi laskurin nousu).
    pub fn record(&mut self, reflection: Reflection) {
        match reflection.kind {
            ReflectionKind::Merged => self.merged += 1,
            ReflectionKind::Dropped => self.dropped += 1,
            ReflectionKind::DateAbsolutized => self.dates_absolutized += 1,
            ReflectionKind::Strengthened => self.strengthened += 1,
            ReflectionKind::Archived => self.archived += 1,
        }
        self.reflections.push(reflection);
    }

    /// Tekikö unijakso mitään muutoksia.
    #[must_use]
    pub fn made_changes(&self) -> bool {
        self.merged > 0
            || self.dropped > 0
            || self.archived > 0
            || self.strengthened > 0
            || self.dates_absolutized > 0
    }

    /// Reflektioiden kokonaismäärä (sama kuin laskureiden summa).
    #[must_use]
    pub fn total_actions(&self) -> usize {
        self.merged + self.dropped + self.archived + self.strengthened + self.dates_absolutized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time;

    #[test]
    fn reflection_kind_as_str_matches_serde() {
        let kinds = [
            ReflectionKind::Merged,
            ReflectionKind::Dropped,
            ReflectionKind::DateAbsolutized,
            ReflectionKind::Strengthened,
            ReflectionKind::Archived,
        ];
        for k in kinds {
            assert_eq!(k.to_string(), k.as_str());
            let json = serde_json::to_string(&k).expect("serialize kind");
            assert_eq!(json, format!("\"{}\"", k.as_str()));
            let back: ReflectionKind = serde_json::from_str(&json).expect("deserialize kind");
            assert_eq!(back, k);
        }
    }

    #[test]
    fn empty_report_made_no_changes() {
        let r = DreamReport::default();
        assert!(!r.made_changes());
        assert_eq!(r.total_actions(), 0);
        assert!(r.ran_at.is_none());
    }

    #[test]
    fn new_sets_ran_at() {
        let now = time::now();
        let r = DreamReport::new(now);
        assert_eq!(r.ran_at, Some(now));
        assert!(!r.made_changes());
    }

    #[test]
    fn record_increments_matching_counter_only() {
        let mut r = DreamReport::default();
        let id = MessageId::new();
        r.record(Reflection::new(ReflectionKind::Merged, id, "m"));
        r.record(Reflection::new(ReflectionKind::Merged, id, "m2"));
        r.record(Reflection::new(ReflectionKind::Dropped, id, "d"));
        r.record(Reflection::new(ReflectionKind::Archived, id, "a"));
        r.record(Reflection::new(ReflectionKind::Strengthened, id, "s"));
        r.record(Reflection::new(ReflectionKind::DateAbsolutized, id, "da"));

        assert_eq!(r.merged, 2);
        assert_eq!(r.dropped, 1);
        assert_eq!(r.archived, 1);
        assert_eq!(r.strengthened, 1);
        assert_eq!(r.dates_absolutized, 1);
        assert_eq!(r.reflections.len(), 6);
        assert_eq!(r.total_actions(), 6);
        assert!(r.made_changes());
    }

    #[test]
    fn counters_stay_in_sync_with_reflections() {
        let mut r = DreamReport::default();
        for _ in 0..10 {
            r.record(Reflection::new(
                ReflectionKind::Merged,
                MessageId::new(),
                "x",
            ));
        }
        assert_eq!(r.merged, r.reflections.len());
        assert_eq!(r.total_actions(), r.reflections.len());
    }

    #[test]
    fn report_serde_roundtrip() {
        let mut r = DreamReport::new(time::now());
        r.record(Reflection::new(
            ReflectionKind::DateAbsolutized,
            MessageId::new(),
            "yesterday → 2026-06-03",
        ));
        let json = serde_json::to_string(&r).expect("serialize");
        let back: DreamReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(r, back);
    }
}
