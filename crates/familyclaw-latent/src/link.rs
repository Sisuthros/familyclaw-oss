//! `RecursiveLink` — dimensio-silta agentti A:n latent-avaruudesta
//! agentti B:n latent-avaruuteen.
//!
//! Eri mallit tuottavat eri ulottuvuuksilla olevia piilotiloja (esim.
//! agentti A 8-ulotteinen, agentti B 12-ulotteinen). Jotta sisarukset
//! voivat vaihtaa piilotiloja, dimensiot on **sillattava** yhteen.
//!
//! ## Tärkeä rajaus (rehellinen luuranko, ei liioittelua)
//! Tämä toteutus tekee vain **yksinkertaisen lineaarisen sovituksen**:
//! lyhennys (truncate) jos lähde on suurempi, tai täyttö nollilla (pad)
//! jos lähde on pienempi. Tämä **säilyttää viestinnän toimivuuden**, muttei
//! ole semanttisesti oppiva projektio — kahden eri mallin latent-avaruudet
//! eivät ole kohdistettuja, joten pelkkä pad/truncate **ei takaa että
//! merkitys säilyy**. Oikea opittu projektiomatriisi (esim. LatentMAS-tyylinen
//! koulutettu sovitus) tulee myöhempänä iteraationa; siihen asti
//! [`crate::channel`] tarjoaa aina teksti-fallbackin.
//!
//! Lisäksi tuetaan **identiteettisilta** (sama lähde- ja kohde-malli):
//! tällöin vektoria ei muuteta lainkaan, koska se on jo oikeassa avaruudessa.

use serde::{Deserialize, Serialize};

use crate::vector::LatentVector;

/// Projektion strategia, jota [`RecursiveLink`] käyttää dimensioiden
/// sovittamiseen lähde- ja kohde-avaruuden välillä.
///
/// Strategia valitaan automaattisesti lähteen ja kohteen dimensiolukujen
/// perusteella, mutta se tallennetaan tulokseen
/// ([`ProjectedLatent::strategy`]) jäljitettävyyttä varten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStrategy {
    /// Lähde- ja kohde-malli ovat samat: vektori siirretään muuttumattomana.
    Identity,
    /// Kohdeavaruus on pienempi: ylimääräiset komponentit pudotetaan.
    Truncate,
    /// Kohdeavaruus on suurempi: puuttuvat komponentit täytetään nollilla.
    Pad,
    /// Lähde- ja kohde-dimensiot ovat yhtä suuret (mutta mallit eri):
    /// komponentit kopioidaan sellaisenaan.
    Resize,
}

/// Yhden projektion tulos: kohdeavaruuteen sillattu [`LatentVector`] sekä
/// metatieto siitä, miten silta tehtiin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectedLatent {
    /// Kohde-malliin sovitettu latent-vektori.
    pub vector: LatentVector,
    /// Käytetty projektiostrategia.
    pub strategy: ProjectionStrategy,
    /// Lähteen alkuperäinen dimensioluku (ennen projektiota).
    pub source_dims: usize,
    /// Kohteen dimensioluku (projektion jälkeen).
    pub target_dims: usize,
    /// `true` jos projektio oli **häviötön** (identiteetti tai pelkkä
    /// nollatäyttö). `false` jos lyhennys hävitti komponentteja.
    pub lossless: bool,
}

/// Lineaarinen dimensio-silta kahden mallin latent-avaruuden välillä.
///
/// `RecursiveLink` kuvaa, että agentti A:n (`source_model`,
/// `source_dims`) piilotiloja voidaan siirtää agentti B:n
/// (`target_model`, `target_dims`) avaruuteen. Nimi *recursive* viittaa
/// design-dokumentin RecursiveMAS-/LatentMAS-juureen; tämä versio on sen
/// ensimmäinen, lineaarinen approksimaatio.
///
/// # Esimerkki
/// ```
/// use familyclaw_latent::link::{RecursiveLink, ProjectionStrategy};
/// use familyclaw_latent::vector::LatentVector;
///
/// // agent_a tuottaa 4-ulotteisen tilan, agent_b odottaa 6-ulotteista.
/// let link = RecursiveLink::new("agent_a/v1", 4, "agent_b/v1", 6);
/// let v = LatentVector::new(vec![1.0, 2.0, 3.0, 4.0], "agent_a/v1");
/// let projected = link.project(&v).expect("dimensiot täsmäävät linkkiin");
/// assert_eq!(projected.vector.len(), 6);
/// assert_eq!(projected.strategy, ProjectionStrategy::Pad);
/// assert!(projected.lossless); // pelkkä nollatäyttö ei hävitä mitään
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecursiveLink {
    /// Lähde-mallin tunniste (`"provider/model"`).
    source_model: String,
    /// Lähde-avaruuden dimensioluku.
    source_dims: usize,
    /// Kohde-mallin tunniste (`"provider/model"`).
    target_model: String,
    /// Kohde-avaruuden dimensioluku.
    target_dims: usize,
}

impl RecursiveLink {
    /// Rakentaa uuden dimensio-sillan lähde-mallista kohde-malliin.
    #[must_use]
    pub fn new(
        source_model: impl Into<String>,
        source_dims: usize,
        target_model: impl Into<String>,
        target_dims: usize,
    ) -> Self {
        Self {
            source_model: source_model.into(),
            source_dims,
            target_model: target_model.into(),
            target_dims,
        }
    }

    /// Lähde-mallin tunniste.
    #[must_use]
    pub fn source_model(&self) -> &str {
        &self.source_model
    }

    /// Kohde-mallin tunniste.
    #[must_use]
    pub fn target_model(&self) -> &str {
        &self.target_model
    }

    /// Lähde-avaruuden dimensioluku.
    #[must_use]
    pub fn source_dims(&self) -> usize {
        self.source_dims
    }

    /// Kohde-avaruuden dimensioluku.
    #[must_use]
    pub fn target_dims(&self) -> usize {
        self.target_dims
    }

    /// Onko tämä identiteettisilta (sama lähde- ja kohde-malli **ja**
    /// sama dimensioluku).
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.source_model == self.target_model && self.source_dims == self.target_dims
    }

    /// Projisoi annetun vektorin kohde-avaruuteen.
    ///
    /// Tekee yksinkertaisen lineaarisen sovituksen:
    /// - sama malli + sama koko → [`ProjectionStrategy::Identity`]
    /// - sama koko, eri malli → [`ProjectionStrategy::Resize`] (kopiointi)
    /// - kohde suurempi → [`ProjectionStrategy::Pad`] (nollatäyttö, häviötön)
    /// - kohde pienempi → [`ProjectionStrategy::Truncate`] (häviöllinen)
    ///
    /// # Errors
    /// Palauttaa [`crate::FamilyClawError::InvalidInput`] jos:
    /// - vektorin `model_id` ei vastaa sillan `source_model`-tunnistetta, tai
    /// - vektorin dimensioluku ei vastaa sillan `source_dims`-arvoa, tai
    /// - vektori sisältää ei-äärellisiä arvoja (`NaN`/`inf`).
    ///
    /// Virhe on tarkoituksellinen signaali kutsujalle palata teksti-fallbackiin.
    pub fn project(&self, vector: &LatentVector) -> crate::Result<ProjectedLatent> {
        if vector.model_id != self.source_model {
            return Err(crate::FamilyClawError::invalid_input(format!(
                "latent vector model '{}' does not match link source model '{}'",
                vector.model_id, self.source_model
            )));
        }
        if vector.len() != self.source_dims {
            return Err(crate::FamilyClawError::invalid_input(format!(
                "latent vector has {} dims but link source expects {}",
                vector.len(),
                self.source_dims
            )));
        }
        if !vector.is_finite() {
            return Err(crate::FamilyClawError::invalid_input(
                "latent vector contains non-finite values (NaN/inf); cannot project",
            ));
        }

        let source_dims = self.source_dims;
        let target_dims = self.target_dims;

        let (dims, strategy, lossless) = match target_dims.cmp(&source_dims) {
            std::cmp::Ordering::Equal => {
                let strategy = if self.source_model == self.target_model {
                    ProjectionStrategy::Identity
                } else {
                    ProjectionStrategy::Resize
                };
                (vector.dims.clone(), strategy, true)
            }
            std::cmp::Ordering::Greater => {
                // Pad: kopioi lähde + täytä loppu nollilla. Häviötön.
                let mut dims = Vec::with_capacity(target_dims);
                dims.extend_from_slice(&vector.dims);
                dims.resize(target_dims, 0.0);
                (dims, ProjectionStrategy::Pad, true)
            }
            std::cmp::Ordering::Less => {
                // Truncate: pidä vain ensimmäiset `target_dims` komponenttia.
                // Häviöllinen — osa piilotilasta katoaa.
                let dims = vector.dims[..target_dims].to_vec();
                (dims, ProjectionStrategy::Truncate, false)
            }
        };

        Ok(ProjectedLatent {
            vector: LatentVector::new(dims, self.target_model.clone()),
            strategy,
            source_dims,
            target_dims,
            lossless,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec_a(dims: Vec<f32>) -> LatentVector {
        LatentVector::new(dims, "agent_a/v1")
    }

    #[test]
    fn identity_link_passes_through_unchanged() {
        let link = RecursiveLink::new("agent_a/v1", 3, "agent_a/v1", 3);
        assert!(link.is_identity());
        let v = vec_a(vec![1.0, 2.0, 3.0]);
        let p = link.project(&v).expect("identity projects");
        assert_eq!(p.strategy, ProjectionStrategy::Identity);
        assert_eq!(p.vector.dims, vec![1.0, 2.0, 3.0]);
        assert_eq!(p.vector.model_id, "agent_a/v1");
        assert!(p.lossless);
    }

    #[test]
    fn same_size_different_model_resizes_losslessly() {
        let link = RecursiveLink::new("agent_a/v1", 3, "agent_b/v1", 3);
        assert!(!link.is_identity());
        let p = link.project(&vec_a(vec![1.0, 2.0, 3.0])).expect("ok");
        assert_eq!(p.strategy, ProjectionStrategy::Resize);
        assert_eq!(p.vector.dims, vec![1.0, 2.0, 3.0]);
        assert_eq!(p.vector.model_id, "agent_b/v1");
        assert!(p.lossless);
    }

    #[test]
    fn pad_extends_with_zeros_and_is_lossless() {
        let link = RecursiveLink::new("agent_a/v1", 2, "agent_b/v1", 5);
        let p = link.project(&vec_a(vec![7.0, 8.0])).expect("ok");
        assert_eq!(p.strategy, ProjectionStrategy::Pad);
        assert_eq!(p.vector.dims, vec![7.0, 8.0, 0.0, 0.0, 0.0]);
        assert_eq!(p.target_dims, 5);
        assert_eq!(p.source_dims, 2);
        assert!(p.lossless);
    }

    #[test]
    fn truncate_drops_tail_and_is_lossy() {
        let link = RecursiveLink::new("agent_a/v1", 5, "agent_b/v1", 2);
        let p = link
            .project(&vec_a(vec![1.0, 2.0, 3.0, 4.0, 5.0]))
            .expect("ok");
        assert_eq!(p.strategy, ProjectionStrategy::Truncate);
        assert_eq!(p.vector.dims, vec![1.0, 2.0]);
        assert!(!p.lossless);
    }

    #[test]
    fn rejects_mismatched_source_model() {
        let link = RecursiveLink::new("agent_a/v1", 3, "agent_b/v1", 3);
        let wrong = LatentVector::new(vec![1.0, 2.0, 3.0], "agent_c/v1");
        let err = link.project(&wrong).expect_err("model mismatch must fail");
        assert!(matches!(err, crate::FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn rejects_mismatched_source_dims() {
        let link = RecursiveLink::new("agent_a/v1", 3, "agent_b/v1", 3);
        let err = link
            .project(&vec_a(vec![1.0, 2.0]))
            .expect_err("dim mismatch must fail");
        assert!(matches!(err, crate::FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn rejects_non_finite_vector() {
        let link = RecursiveLink::new("agent_a/v1", 2, "agent_b/v1", 2);
        let err = link
            .project(&vec_a(vec![1.0, f32::NAN]))
            .expect_err("nan must fail");
        assert!(matches!(err, crate::FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn accessors_report_link_shape() {
        let link = RecursiveLink::new("agent_a/v1", 4, "agent_b/v1", 6);
        assert_eq!(link.source_model(), "agent_a/v1");
        assert_eq!(link.target_model(), "agent_b/v1");
        assert_eq!(link.source_dims(), 4);
        assert_eq!(link.target_dims(), 6);
    }

    #[test]
    fn zero_dim_link_handles_empty_vectors() {
        // Reunaehto: tyhjä lähde, tyhjä kohde.
        let link = RecursiveLink::new("agent_a/v1", 0, "agent_b/v1", 0);
        let p = link.project(&vec_a(vec![])).expect("empty ok");
        assert!(p.vector.is_empty());
        assert_eq!(p.strategy, ProjectionStrategy::Resize);
    }

    #[test]
    fn pad_from_empty_source() {
        // Reunaehto: 0 → N pad tuottaa pelkkiä nollia.
        let link = RecursiveLink::new("agent_a/v1", 0, "agent_b/v1", 3);
        let p = link.project(&vec_a(vec![])).expect("ok");
        assert_eq!(p.strategy, ProjectionStrategy::Pad);
        assert_eq!(p.vector.dims, vec![0.0, 0.0, 0.0]);
        assert!(p.lossless);
    }

    #[test]
    fn projected_latent_serde_roundtrip() {
        let link = RecursiveLink::new("agent_a/v1", 2, "agent_b/v1", 3);
        let p = link.project(&vec_a(vec![1.0, 2.0])).expect("ok");
        let json = serde_json::to_string(&p).expect("serialize");
        let back: ProjectedLatent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, back);
    }
}
