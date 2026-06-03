//! Latent-vektorit — agentin hidden-state-representaatio.
//!
//! [`LatentVector`] kapseloi yhden agentin *piilotilan* (hidden state):
//! mallin viimeisen kerroksen aktivaatiot kelluvina lukuina, sekä tiedon
//! siitä **mikä malli** sen tuotti ([`LatentVector::model_id`]). Mallitunniste
//! on kriittinen, koska eri mallien latent-avaruudet eivät ole keskenään
//! vertailukelpoisia ilman projektiota (ks. [`crate::link`]).
//!
//! ## Tutkimuskonteksti
//! Tämä on rehellinen *luuranko* LatentMAS-tyyppiselle (ICML 2026)
//! sisarusviestinnälle. Vektori itsessään on pelkkä numeerinen kantaja —
//! se ei väitä ymmärtävänsä, mitä piilotila merkitsee. Todellinen oppiva
//! tulkinta/projektio tulee myöhemmin; tässä vaiheessa pidämme rakenteen
//! yksinkertaisena ja todennettavissa olevana.

use serde::{Deserialize, Serialize};

/// Agentin hidden-state-representaatio: kelluvien lukujen vektori sekä
/// sen tuottaneen mallin tunniste.
///
/// `model_id` on vapaamuotoinen mallitunniste muodossa `"provider/model"`
/// (esim. `"agent-a-model/v1"`). Sitä käytetään tarkistamaan, voiko kaksi
/// vektoria yhdistää suoraan vai tarvitaanko [`crate::link::RecursiveLink`].
///
/// # OSS-raja (KERROS A)
/// Tämä tyyppi ei kovakoodaa todellisia mallinimiä eikä perheenjäsenten
/// tietoja — `model_id` annetaan aina ajonaikaisesti.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatentVector {
    /// Piilotilan numeeriset komponentit (mallin viimeisen kerroksen
    /// aktivaatiot tai vastaava tiivistetty edustus).
    pub dims: Vec<f32>,
    /// Vektorin tuottaneen mallin tunniste, muodossa `"provider/model"`.
    pub model_id: String,
}

impl LatentVector {
    /// Luo uuden latent-vektorin annetuista komponenteista ja mallitunnisteesta.
    ///
    /// Tyhjä `dims` on sallittu (edustaa "ei piilotilaa") — yhteensopivuus-
    /// ja fallback-logiikka käsittelevät sen turvallisesti.
    #[must_use]
    pub fn new(dims: Vec<f32>, model_id: impl Into<String>) -> Self {
        Self {
            dims,
            model_id: model_id.into(),
        }
    }

    /// Vektorin dimensioluku (komponenttien määrä).
    #[must_use]
    pub fn len(&self) -> usize {
        self.dims.len()
    }

    /// Onko vektori tyhjä (ei yhtään komponenttia).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dims.is_empty()
    }

    /// Onko vektorin tuottanut malli sama kuin `other`-mallitunniste.
    ///
    /// Tämä on nopea ennakkotarkistus: jos mallit ovat samat, vektorit
    /// elävät samassa latent-avaruudessa eikä projektiota tarvita.
    #[must_use]
    pub fn same_model(&self, other_model_id: &str) -> bool {
        self.model_id == other_model_id
    }

    /// Onko vektori numeerisesti terve: kaikki komponentit ovat äärellisiä
    /// (ei `NaN`, ei ääretön).
    ///
    /// Epäterve piilotila on signaali siitä, että latent-siirto kannattaa
    /// hylätä ja palata teksti-fallbackiin (ks. [`crate::channel`]).
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.dims.iter().all(|x| x.is_finite())
    }

    /// L2-normi (euklidinen pituus). Hyödyllinen sanity-tarkistuksiin ja
    /// projektion vaikutuksen mittaamiseen.
    #[must_use]
    pub fn l2_norm(&self) -> f32 {
        self.dims.iter().map(|x| x * x).sum::<f32>().sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_dims_and_model() {
        let v = LatentVector::new(vec![1.0, 2.0, 3.0], "agent-a/v1");
        assert_eq!(v.len(), 3);
        assert_eq!(v.model_id, "agent-a/v1");
        assert!(!v.is_empty());
    }

    #[test]
    fn empty_vector_is_empty() {
        let v = LatentVector::new(vec![], "agent-a/v1");
        assert!(v.is_empty());
        assert_eq!(v.len(), 0);
    }

    #[test]
    fn same_model_compares_id() {
        let v = LatentVector::new(vec![0.0], "agent-a/v1");
        assert!(v.same_model("agent-a/v1"));
        assert!(!v.same_model("agent-b/v1"));
    }

    #[test]
    fn is_finite_detects_nan_and_inf() {
        assert!(LatentVector::new(vec![1.0, -2.5, 0.0], "m").is_finite());
        assert!(!LatentVector::new(vec![1.0, f32::NAN], "m").is_finite());
        assert!(!LatentVector::new(vec![f32::INFINITY], "m").is_finite());
        assert!(!LatentVector::new(vec![f32::NEG_INFINITY], "m").is_finite());
    }

    #[test]
    fn empty_vector_is_finite() {
        // Tyhjä vektori on vacuously finite — kaikki nollasta komponentista
        // ovat äärellisiä.
        assert!(LatentVector::new(vec![], "m").is_finite());
    }

    #[test]
    fn l2_norm_of_known_vector() {
        let v = LatentVector::new(vec![3.0, 4.0], "m");
        assert!((v.l2_norm() - 5.0).abs() < 1e-6);
    }

    #[test]
    fn l2_norm_of_empty_is_zero() {
        assert!(LatentVector::new(vec![], "m").l2_norm().abs() < f32::EPSILON);
    }

    #[test]
    fn serde_roundtrip() {
        let v = LatentVector::new(vec![0.5, -0.5, 1.5], "agent-a/v2");
        let json = serde_json::to_string(&v).expect("serialize");
        let back: LatentVector = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(v, back);
    }
}
