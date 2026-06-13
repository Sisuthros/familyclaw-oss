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

/// Kosinisimilariteetti kahden komponenttijonon välillä.
///
/// Palauttaa arvon välillä `[-1.0, 1.0]`, missä `1.0` tarkoittaa identtistä
/// suuntaa (käytetään testeissä mittaamaan, säilyttääkö projektio/sekoitus
/// merkityksen suunnan). Mittaa **suuntaa**, ei pituutta.
///
/// Reunaehdot (palautetaan `0.0`, ei `NaN` — soveltuu pisteytykseen):
/// - eri pituiset jonot (vertailukelvottomat),
/// - tyhjät jonot,
/// - jompikumpi jono on nollavektori (suunta ei määritelty),
/// - ei-äärelliset arvot (`NaN`/`inf`) syötteessä.
///
/// # Esimerkki
/// ```
/// use familyclaw_latent::vector::cosine;
/// assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
/// assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6); // ortogonaaliset
/// ```
#[must_use]
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    if !a.iter().chain(b.iter()).all(|x| x.is_finite()) {
        return 0.0;
    }

    let mut dot = 0.0_f32;
    let mut norm_a = 0.0_f32;
    let mut norm_b = 0.0_f32;
    for (&x, &y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom <= 0.0 {
        return 0.0;
    }

    // Numeerinen kiinnitys [-1, 1]:een (liukulukuvirheet voivat ylittää rajan).
    (dot / denom).clamp(-1.0, 1.0)
}

/// Sekoittaa kaksi samanmittaista vektoria lineaarisesti annetulla
/// sekoitusvoimalla (Direct Semantic Communication -tyylinen ~30 % blend).
///
/// Tulos on komponenteittain `original * (1 - strength) + other * strength`.
/// `strength` kiinnitetään välille `[0.0, 1.0]`:
/// - `0.0` → palauttaa `original`:n muuttumattomana,
/// - `1.0` → palauttaa `other`:n (kunhan pituudet täsmäävät),
/// - `0.3` → Direct Semantic -paperin oletussekoitus.
///
/// Tuloksen `model_id` peritään `original`:lta — sekoitus elää lähettäjän
/// avaruudessa, eikä väitä vaihtaneensa mallia.
///
/// # Errors
/// Palauttaa [`crate::FamilyClawError::InvalidInput`] jos vektorit ovat eri
/// mittaiset (sekoitus vaatii kohdistetut, samandimensioiset komponentit).
pub fn blend(
    original: &LatentVector,
    other: &LatentVector,
    strength: f32,
) -> crate::Result<LatentVector> {
    if original.len() != other.len() {
        return Err(crate::FamilyClawError::invalid_input(format!(
            "blend requires equal dims but got {} and {}",
            original.len(),
            other.len()
        )));
    }

    let s = strength.clamp(0.0, 1.0);
    let inv = 1.0 - s;
    let dims = original
        .dims
        .iter()
        .zip(other.dims.iter())
        .map(|(&o, &t)| o * inv + t * s)
        .collect();

    Ok(LatentVector::new(dims, original.model_id.clone()))
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

    #[test]
    fn cosine_of_identical_is_one() {
        let a = [1.0, 2.0, 3.0];
        assert!((cosine(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_orthogonal_is_zero() {
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_opposite_is_negative_one() {
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn cosine_handles_edge_cases_without_nan() {
        // Eri pituus, tyhjä, nollavektori, ei-äärellinen → 0.0 (ei NaN).
        assert!(cosine(&[1.0, 2.0], &[1.0]).abs() < f32::EPSILON);
        assert!(cosine(&[], &[]).abs() < f32::EPSILON);
        assert!(cosine(&[0.0, 0.0], &[1.0, 1.0]).abs() < f32::EPSILON);
        assert!(cosine(&[f32::NAN, 1.0], &[1.0, 1.0]).abs() < f32::EPSILON);
        assert!(!cosine(&[0.0], &[0.0]).is_nan());
    }

    #[test]
    fn cosine_is_scale_invariant() {
        // Kosini mittaa suuntaa, ei pituutta: skaalaus ei muuta tulosta.
        let base = cosine(&[1.0, 2.0, 3.0], &[2.0, 1.0, 0.0]);
        let scaled = cosine(&[10.0, 20.0, 30.0], &[2.0, 1.0, 0.0]);
        assert!((base - scaled).abs() < 1e-6);
    }

    #[test]
    fn blend_at_zero_returns_original() {
        let a = LatentVector::new(vec![1.0, 2.0, 3.0], "agent_a/v1");
        let b = LatentVector::new(vec![9.0, 9.0, 9.0], "agent_b/v1");
        let blended = blend(&a, &b, 0.0).expect("equal dims");
        assert_eq!(blended.dims, vec![1.0, 2.0, 3.0]);
        // model_id peritään originalilta.
        assert_eq!(blended.model_id, "agent_a/v1");
    }

    #[test]
    fn blend_at_one_returns_other_values() {
        let a = LatentVector::new(vec![1.0, 2.0], "agent_a/v1");
        let b = LatentVector::new(vec![9.0, 8.0], "agent_b/v1");
        let blended = blend(&a, &b, 1.0).expect("equal dims");
        assert_eq!(blended.dims, vec![9.0, 8.0]);
        // model_id pysyy yhä originalin avaruudessa.
        assert_eq!(blended.model_id, "agent_a/v1");
    }

    #[test]
    fn blend_at_thirty_percent_is_weighted_mix() {
        // Direct Semantic -paperin oletus: 30 % sekoitus.
        let a = LatentVector::new(vec![0.0, 0.0], "agent_a/v1");
        let b = LatentVector::new(vec![10.0, 100.0], "agent_b/v1");
        let blended = blend(&a, &b, 0.3).expect("equal dims");
        assert!((blended.dims[0] - 3.0).abs() < 1e-5);
        assert!((blended.dims[1] - 30.0).abs() < 1e-4);
    }

    #[test]
    fn blend_clamps_out_of_range_strength() {
        let a = LatentVector::new(vec![1.0], "agent_a/v1");
        let b = LatentVector::new(vec![5.0], "agent_b/v1");
        // < 0 kiinnittyy 0:aan (original), > 1 kiinnittyy 1:een (other).
        assert_eq!(blend(&a, &b, -2.0).expect("ok").dims, vec![1.0]);
        assert_eq!(blend(&a, &b, 7.0).expect("ok").dims, vec![5.0]);
    }

    #[test]
    fn blend_rejects_mismatched_dims() {
        let a = LatentVector::new(vec![1.0, 2.0], "agent_a/v1");
        let b = LatentVector::new(vec![1.0], "agent_b/v1");
        let err = blend(&a, &b, 0.3).expect_err("dim mismatch must fail");
        assert!(matches!(err, crate::FamilyClawError::InvalidInput(_)));
    }
}
