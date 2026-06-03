//! Unijakson säätöparametrit ([`DreamConfig`]).
//!
//! Konfiguraatio kerää yhteen kaikki konsolidaation kynnysarvot. Oletukset
//! on johdettu `FamilyClaw` v2 -designista (§2.3, §5) ja
//! `familyclaw-memory`-craten Ebbinghaus-malliasta, ei arvattu. Kaikki kentät
//! puristetaan rakennettaessa järkeviin rajoihin, joten kelvoton syöte ei voi
//! tuottaa rikkinäistä unijaksoa.

use serde::{Deserialize, Serialize};

/// Unijakson kynnysarvot ja kytkimet.
///
/// Rakenna [`DreamConfig::default`]:lla (suositeltu) tai
/// [`DreamConfig::new`]:llä ja säädä builder-tyylillä. Arvot ovat puhtaita
/// liukulukukynnyksiä — ei perhe-/kalibrointitietoa (KERROS A, OSS).
///
/// Neljä `bool`-kytkintä ovat tarkoituksella itsenäisiä vaihe-lippuja (kukin
/// kytkee yhden konsolidaatiovaiheen päälle/pois), ei tilakone — siksi
/// `struct_excessive_bools` sallitaan tässä.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DreamConfig {
    /// Jaccard-kynnys jolla kaksi muistoa katsotaan duplikaateiksi
    /// (`0.0..=1.0`). Korkeampi = tiukempi (vaatii enemmän päällekkäisyyttä).
    pub merge_similarity: f32,

    /// Retention-kynnys jonka alittavat muistot arkistoidaan unessa
    /// (`0.0..=1.0`). Design §2.3: matalan tärkeyden muistot (R < 0.05)
    /// arkistoituvat.
    pub archive_below_retention: f32,

    /// Tärkeyskynnys jonka ylittävät muistot vahvistetaan unessa
    /// (`0.0..=1.0`). Design §2.3: korkean importancen muistot vahvistuvat.
    pub strengthen_above_importance: f32,

    /// Suoritetaanko duplikaattien yhdistäminen.
    pub merge_duplicates: bool,
    /// Suoritetaanko ristiriitaisten/vanhentuneiden pudottaminen.
    pub drop_contradicted: bool,
    /// Suoritetaanko suhteellisten päiväysten absolutisointi.
    pub absolutize_dates: bool,
    /// Suoritetaanko tärkeiden vahvistus ja matalien arkistointi.
    pub consolidate: bool,
}

impl DreamConfig {
    /// Oletus duplikaattikynnykselle (vahva, mutta ei identtisyysvaatimus).
    pub const DEFAULT_MERGE_SIMILARITY: f32 = 0.85;
    /// Oletus arkistointiretentiolle (design §2.3: R < 0.05).
    pub const DEFAULT_ARCHIVE_BELOW_RETENTION: f32 = 0.05;
    /// Oletus vahvistuskynnykselle (tärkeät muistot).
    pub const DEFAULT_STRENGTHEN_ABOVE_IMPORTANCE: f32 = 0.6;

    /// Rakentaa konfiguraation kolmesta kynnyksestä, kaikki vaiheet päällä.
    ///
    /// Kentät puristetaan välille `0.0..=1.0`; kelvoton (NaN/ääretön) korvataan
    /// vastaavalla oletuksella.
    #[must_use]
    pub fn new(
        merge_similarity: f32,
        archive_below_retention: f32,
        strengthen_above_importance: f32,
    ) -> Self {
        Self {
            merge_similarity: clamp_unit(merge_similarity, Self::DEFAULT_MERGE_SIMILARITY),
            archive_below_retention: clamp_unit(
                archive_below_retention,
                Self::DEFAULT_ARCHIVE_BELOW_RETENTION,
            ),
            strengthen_above_importance: clamp_unit(
                strengthen_above_importance,
                Self::DEFAULT_STRENGTHEN_ABOVE_IMPORTANCE,
            ),
            merge_duplicates: true,
            drop_contradicted: true,
            absolutize_dates: true,
            consolidate: true,
        }
    }

    /// Asettaa duplikaattikynnyksen (puristetaan `0.0..=1.0`).
    #[must_use]
    pub fn with_merge_similarity(mut self, v: f32) -> Self {
        self.merge_similarity = clamp_unit(v, Self::DEFAULT_MERGE_SIMILARITY);
        self
    }

    /// Asettaa arkistointiretention kynnyksen (puristetaan `0.0..=1.0`).
    #[must_use]
    pub fn with_archive_below_retention(mut self, v: f32) -> Self {
        self.archive_below_retention = clamp_unit(v, Self::DEFAULT_ARCHIVE_BELOW_RETENTION);
        self
    }

    /// Asettaa vahvistuskynnyksen (puristetaan `0.0..=1.0`).
    #[must_use]
    pub fn with_strengthen_above_importance(mut self, v: f32) -> Self {
        self.strengthen_above_importance = clamp_unit(v, Self::DEFAULT_STRENGTHEN_ABOVE_IMPORTANCE);
        self
    }

    /// Kytkee duplikaattien yhdistämisen päälle/pois.
    #[must_use]
    pub const fn merging(mut self, on: bool) -> Self {
        self.merge_duplicates = on;
        self
    }

    /// Kytkee ristiriitaisten pudottamisen päälle/pois.
    #[must_use]
    pub const fn dropping_contradicted(mut self, on: bool) -> Self {
        self.drop_contradicted = on;
        self
    }

    /// Kytkee päiväysten absolutisoinnin päälle/pois.
    #[must_use]
    pub const fn absolutizing_dates(mut self, on: bool) -> Self {
        self.absolutize_dates = on;
        self
    }

    /// Kytkee konsolidaation (vahvistus + arkistointi) päälle/pois.
    #[must_use]
    pub const fn consolidating(mut self, on: bool) -> Self {
        self.consolidate = on;
        self
    }
}

impl Default for DreamConfig {
    /// Designin mukaiset oletukset, kaikki vaiheet päällä.
    fn default() -> Self {
        Self::new(
            Self::DEFAULT_MERGE_SIMILARITY,
            Self::DEFAULT_ARCHIVE_BELOW_RETENTION,
            Self::DEFAULT_STRENGTHEN_ABOVE_IMPORTANCE,
        )
    }
}

/// Puristaa arvon välille `0.0..=1.0`; kelvoton (NaN/ääretön) → `fallback`.
fn clamp_unit(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    // Tarkka f32-vertailu sallittu — vakioidut kynnykset.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn default_matches_design_constants() {
        let c = DreamConfig::default();
        assert_eq!(c.merge_similarity, 0.85);
        assert_eq!(c.archive_below_retention, 0.05);
        assert_eq!(c.strengthen_above_importance, 0.6);
        assert!(c.merge_duplicates);
        assert!(c.drop_contradicted);
        assert!(c.absolutize_dates);
        assert!(c.consolidate);
    }

    #[test]
    fn new_clamps_out_of_range() {
        let c = DreamConfig::new(5.0, -1.0, 2.0);
        assert_eq!(c.merge_similarity, 1.0);
        assert_eq!(c.archive_below_retention, 0.0);
        assert_eq!(c.strengthen_above_importance, 1.0);
    }

    #[test]
    fn new_falls_back_on_invalid() {
        let c = DreamConfig::new(f32::NAN, f32::INFINITY, f32::NEG_INFINITY);
        assert_eq!(c.merge_similarity, DreamConfig::DEFAULT_MERGE_SIMILARITY);
        assert_eq!(
            c.archive_below_retention,
            DreamConfig::DEFAULT_ARCHIVE_BELOW_RETENTION
        );
        assert_eq!(
            c.strengthen_above_importance,
            DreamConfig::DEFAULT_STRENGTHEN_ABOVE_IMPORTANCE
        );
    }

    #[test]
    fn builder_setters_clamp() {
        let c = DreamConfig::default()
            .with_merge_similarity(0.9)
            .with_archive_below_retention(0.1)
            .with_strengthen_above_importance(0.7);
        assert_eq!(c.merge_similarity, 0.9);
        assert_eq!(c.archive_below_retention, 0.1);
        assert_eq!(c.strengthen_above_importance, 0.7);

        let clamped = DreamConfig::default().with_merge_similarity(99.0);
        assert_eq!(clamped.merge_similarity, 1.0);
    }

    #[test]
    fn phase_toggles() {
        let c = DreamConfig::default()
            .merging(false)
            .dropping_contradicted(false)
            .absolutizing_dates(false)
            .consolidating(false);
        assert!(!c.merge_duplicates);
        assert!(!c.drop_contradicted);
        assert!(!c.absolutize_dates);
        assert!(!c.consolidate);
    }

    #[test]
    fn serde_roundtrip() {
        let c = DreamConfig::default().with_merge_similarity(0.77);
        let json = serde_json::to_string(&c).expect("serialize");
        let back: DreamConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(c, back);
    }
}
