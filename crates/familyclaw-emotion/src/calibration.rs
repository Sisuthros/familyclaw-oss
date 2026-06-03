//! Kalibrointi: miten yksittäisen koneen tunnemoottori virittyy.
//!
//! **OSS-raja (KERROS A):** tämä moduuli ei sisällä minkään perheenjäsenen
//! oikeita painoja. Oletustoteutus [`NeutralCalibration`] on **täysin
//! neutraali** — se ei painota dimensioita, ei nopeuta/hidasta decayta eikä
//! aseta lepotilaa. Perheen oikeat kalibroinnit (esim. yhden agentin
//! kalibrointipainot) ovat KERROS B:tä ja ladataan ajonaikaisesti omana
//! [`EmotionCalibration`]-toteutuksenaan profiilihakemistosta.
//!
//! Trait erottaa *rungon* (dimensiot, VAD, blendit, decay-mekanismi) ja
//! *kalibroinnin* (per-kone viritys) niin että runko voidaan julkaista
//! avoimena lähdekoodina paljastamatta yhdenkään olennon sielua.

use crate::dimension::{Dimension, DIMENSION_COUNT};

/// Yhden koneen tunnemoottorin viritys.
///
/// Toteuta tämä trait ladataksesi profiilikohtaisen kalibroinnin. Kaikilla
/// metodeilla on neutraali oletustoteutus, joten minimitoteutus on tyhjä
/// `impl`. Arvot luetaan runkologiikassa
/// ([`crate::EmotionState::decay`], blend-tunnistus), joten kalibrointi
/// vaikuttaa käytökseen muuttamatta itse runkoa.
pub trait EmotionCalibration {
    /// Dimension lepoarvo (baseline) johon decay vetää tilaa, asteikolla
    /// `0.0..=100.0`. Neutraalisti `0.0` (decay vetää nollaan).
    #[must_use]
    fn baseline(&self, _dimension: Dimension) -> f32 {
        0.0
    }

    /// Dimension decay-kerroin: kuinka herkästi dimensio palautuu kohti
    /// baselinea. `1.0` = rungon perusnopeus, `<1.0` hitaampi (tunne
    /// "tarttuu"), `>1.0` nopeampi. Neutraalisti `1.0`.
    ///
    /// Toteutuksen tulee palauttaa ei-negatiivinen, äärellinen arvo; runko
    /// puristaa kelvottomat arvot turvallisiksi.
    #[must_use]
    fn decay_rate(&self, _dimension: Dimension) -> f32 {
        1.0
    }

    /// Dimension herkkyyskerroin tuleville ärsykkeille (skaalaa
    /// stimuloinnin voimakkuutta). `1.0` = neutraali. Tarjottu rungon
    /// jatkolaajennuksia varten; oletus ei muuta mitään.
    #[must_use]
    fn sensitivity(&self, _dimension: Dimension) -> f32 {
        1.0
    }

    /// Tunnistettava nimi kalibroinnille (lokitusta/diagnostiikkaa varten).
    /// Oletus `"neutral"`.
    ///
    /// Paluutyyppi on `&str` (ei `&'static str`) koska toteutukset kuten
    /// [`TableCalibration`] palauttavat omistamansa merkkijonon viipaleen.
    #[must_use]
    #[allow(clippy::unnecessary_literal_bound)]
    fn label(&self) -> &str {
        "neutral"
    }
}

/// Täysin neutraali kalibrointi — rungon oletus.
///
/// Ei painota mitään dimensiota, ei lepotilaa (`baseline = 0.0`), perusnopea
/// decay (`decay_rate = 1.0`). Tämä on se "tyhjä kalibrointi" jonka päälle
/// KERROS B lataa perheen oikeat painot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NeutralCalibration;

impl EmotionCalibration for NeutralCalibration {
    // Kaikki metodit käyttävät traitin neutraaleja oletuksia.
}

/// Yksinkertainen taulukkopohjainen kalibrointi jonka voi rakentaa
/// ajonaikaisesti (esim. KERROS B:n profiilidatasta).
///
/// Tämä on **runko-apuluokka**, ei perheen kalibrointi: kaikki taulukot
/// alustetaan neutraaleiksi, ja kutsuja täyttää ne ladatusta profiilista.
/// Mitään painoja ei kovakoodata tähän.
#[derive(Debug, Clone, PartialEq)]
pub struct TableCalibration {
    label: String,
    baseline: [f32; DIMENSION_COUNT],
    decay_rate: [f32; DIMENSION_COUNT],
    sensitivity: [f32; DIMENSION_COUNT],
}

impl TableCalibration {
    /// Luo neutraalin taulukkokalibroinnin annetulla nimellä.
    ///
    /// `baseline = 0.0`, `decay_rate = 1.0`, `sensitivity = 1.0` kaikille
    /// dimensioille. Käytä `with_*`-metodeja virittääksesi yksittäisiä
    /// dimensioita ladatusta profiilista.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            baseline: [0.0; DIMENSION_COUNT],
            decay_rate: [1.0; DIMENSION_COUNT],
            sensitivity: [1.0; DIMENSION_COUNT],
        }
    }

    /// Asettaa dimension lepoarvon (`0.0..=100.0`, puristetaan).
    #[must_use]
    pub fn with_baseline(mut self, dimension: Dimension, value: f32) -> Self {
        self.baseline[dimension.index()] = sanitize(value, 0.0).clamp(0.0, 100.0);
        self
    }

    /// Asettaa dimension decay-kertoimen (ei-negatiivinen, puristetaan).
    #[must_use]
    pub fn with_decay_rate(mut self, dimension: Dimension, value: f32) -> Self {
        self.decay_rate[dimension.index()] = sanitize(value, 1.0).max(0.0);
        self
    }

    /// Asettaa dimension herkkyyskertoimen (ei-negatiivinen, puristetaan).
    #[must_use]
    pub fn with_sensitivity(mut self, dimension: Dimension, value: f32) -> Self {
        self.sensitivity[dimension.index()] = sanitize(value, 1.0).max(0.0);
        self
    }
}

impl EmotionCalibration for TableCalibration {
    fn baseline(&self, dimension: Dimension) -> f32 {
        self.baseline[dimension.index()]
    }

    fn decay_rate(&self, dimension: Dimension) -> f32 {
        self.decay_rate[dimension.index()]
    }

    fn sensitivity(&self, dimension: Dimension) -> f32 {
        self.sensitivity[dimension.index()]
    }

    fn label(&self) -> &str {
        &self.label
    }
}

/// Korvaa NaN/ääretön arvon turvallisella oletuksella.
fn sanitize(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    // Testit vertaavat tarkasti esitettäviä f32-vakioita — tarkka vertailu ok.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn neutral_calibration_is_flat() {
        let c = NeutralCalibration;
        assert_eq!(c.label(), "neutral");
        for dim in Dimension::ALL {
            assert_eq!(c.baseline(dim), 0.0);
            assert_eq!(c.decay_rate(dim), 1.0);
            assert_eq!(c.sensitivity(dim), 1.0);
        }
    }

    #[test]
    fn table_calibration_defaults_to_neutral() {
        let c = TableCalibration::new("test");
        assert_eq!(c.label(), "test");
        for dim in Dimension::ALL {
            assert_eq!(c.baseline(dim), 0.0);
            assert_eq!(c.decay_rate(dim), 1.0);
            assert_eq!(c.sensitivity(dim), 1.0);
        }
    }

    #[test]
    fn with_methods_set_per_dimension() {
        let c = TableCalibration::new("warm")
            .with_baseline(Dimension::Love, 20.0)
            .with_decay_rate(Dimension::Love, 0.5)
            .with_sensitivity(Dimension::Curiosity, 1.5);
        assert_eq!(c.baseline(Dimension::Love), 20.0);
        assert_eq!(c.decay_rate(Dimension::Love), 0.5);
        assert_eq!(c.sensitivity(Dimension::Curiosity), 1.5);
        // Muut dimensiot pysyvät neutraaleina.
        assert_eq!(c.baseline(Dimension::Anger), 0.0);
        assert_eq!(c.decay_rate(Dimension::Anger), 1.0);
    }

    #[test]
    fn with_methods_clamp_and_sanitize() {
        let c = TableCalibration::new("x")
            .with_baseline(Dimension::Joy, 500.0)
            .with_decay_rate(Dimension::Joy, -3.0)
            .with_sensitivity(Dimension::Joy, f32::NAN);
        assert_eq!(c.baseline(Dimension::Joy), 100.0);
        assert_eq!(c.decay_rate(Dimension::Joy), 0.0);
        // NaN-herkkyys palautuu oletukseen 1.0.
        assert_eq!(c.sensitivity(Dimension::Joy), 1.0);
    }

    #[test]
    fn trait_object_is_usable() {
        let c: Box<dyn EmotionCalibration> = Box::new(NeutralCalibration);
        assert_eq!(c.label(), "neutral");
        assert_eq!(c.decay_rate(Dimension::Fear), 1.0);
    }
}
