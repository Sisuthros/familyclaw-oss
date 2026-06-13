//! `VectorTranslator` — cross-model latent-vektorin *kääntäjä* lähettäjän
//! mallin avaruudesta vastaanottajan mallin avaruuteen.
//!
//! ## Mihin tämä eroaa [`crate::link::RecursiveLink`]:istä?
//! `RecursiveLink` tekee pelkän **dimensio-sovituksen** (pad/truncate/resize):
//! se ei kosketa arvoihin. `VectorTranslator` lisää sen päälle **konfiguroitavan
//! lineaarisen projektion** — joko komponenttikohtainen `scale`/`offset` tai
//! täysi `matrix` — joka approksimoi avaruuksien välistä karttaa.
//!
//! ## Tärkeä rehellisyysraja (ei liioittelua)
//! Tämä on MVP: **konfiguroitava lineaarinen projektio, EI koulutettu enkooderi.**
//! Direct Semantic Communication -paperi (2025) käyttää opetettua dual-encoder-
//! kääntäjää (~0.538 kosini) ja 30 % sekoitusta. Tässä matriisi/skaalaus annetaan
//! ulkoa — se mahdollistaa *rakenteen* ja testattavuuden, muttei väitä oppineensa
//! semanttista kohdistusta. Identiteettiprojektio (oletus) säilyttää vektorin
//! sellaisenaan, jolloin sama-malli-käännös on häviötön.
//!
//! Dimensioiden eroavuus käsitellään **deterministisesti** (truncate/pad ennen
//! lineaarista karttaa) ja häviöllisyydestä kirjataan
//! [`FallbackReason::ProjectionFailed`] tuloksen mukaan.
//!
//! ## OSS-raja (KERROS A)
//! Ei kovakoodattuja mallinimiä — kaikki mallitunnisteet ja kertoimet annetaan
//! ajonaikaisesti. Esimerkit käyttävät geneerisiä nimiä (`agent_a`, `agent_b`).

use serde::{Deserialize, Serialize};

use crate::channel::{FallbackReason, ReceiverProfile};
use crate::link::{ProjectedLatent, ProjectionStrategy};
use crate::vector::LatentVector;

/// Konfiguroitava lineaarinen kartta lähettäjän avaruudesta vastaanottajan
/// avaruuteen. MVP — annettu ulkoa, ei opittu.
///
/// Kaikki muunnokset toimivat **vastaanottajan dimensioluvussa**: lähde
/// sovitetaan ensin kohdekokoon (deterministinen pad/truncate) ja sitten
/// lineaarinen kartta sovelletaan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Projection {
    /// Identiteetti: arvoja ei muuteta (vain dimensio sovitetaan).
    Identity,
    /// Komponenttikohtainen affiininen kartta: `y[i] = scale[i] * x[i] + offset[i]`.
    /// `scale`- ja `offset`-vektorien pituus määrittää kohdedimension.
    ScaleOffset {
        /// Komponenttikohtaiset kertoimet.
        scale: Vec<f32>,
        /// Komponenttikohtaiset siirtymät.
        offset: Vec<f32>,
    },
    /// Täysi matriisikartta `y = M x`, riveittäin (`rows` × `cols`).
    /// `cols` on lähdedimensio (koh-sovituksen jälkeen), `rows` kohdedimensio.
    Matrix {
        /// Rivit; jokaisen rivin pituus on `cols`.
        rows: Vec<Vec<f32>>,
        /// Sarakkeiden määrä (= odotettu lähdedimensio kartan sisäänmenossa).
        cols: usize,
    },
}

impl Projection {
    /// Lähdedimensio, jonka tämä projektio odottaa sisääntulossa.
    ///
    /// `None` = mikä tahansa (identiteetti / komponenttikohtainen kartta
    /// vaatii vain että sisääntulo on jo kohdekokoinen).
    #[must_use]
    fn input_dims(&self) -> Option<usize> {
        match self {
            Self::Identity => None,
            Self::ScaleOffset { scale, .. } => Some(scale.len()),
            Self::Matrix { cols, .. } => Some(*cols),
        }
    }
}

/// Lähettäjän mallin avaruudesta vastaanottajan avaruuteen kääntävä yksikkö.
///
/// Kapseloi lähettäjän mallitunnisteen ja sen lähtödimension sekä
/// [`Projection`]-konfiguraation. [`translate`](VectorTranslator::translate)
/// tuottaa aina [`ProjectedLatent`]:n — viestintä ei katkea, vaikka
/// dimensiot eivät täsmäisi (silloin tehdään deterministinen häviöllinen
/// sovitus ja merkitään se).
///
/// # Esimerkki
/// ```
/// use familyclaw_latent::translate::{Projection, VectorTranslator};
/// use familyclaw_latent::{LatentVector, ReceiverProfile};
///
/// // Sama-malli-käännös identiteetillä säilyttää vektorin.
/// let tr = VectorTranslator::identity("agent_a/v1", 3);
/// let v = LatentVector::new(vec![1.0, 2.0, 3.0], "agent_a/v1");
/// let rx = ReceiverProfile::latent("agent_a/v1", 3);
/// let projected = tr.translate(&v, &rx);
/// assert_eq!(projected.vector.dims, vec![1.0, 2.0, 3.0]);
/// assert!(projected.lossless);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorTranslator {
    /// Lähettäjän mallitunniste (`"provider/model"`).
    sender_model: String,
    /// Lähettäjän lähtödimensio (vektorin odotettu koko ennen käännöstä).
    sender_dims: usize,
    /// Lineaarinen kartta lähettäjän → vastaanottajan avaruuteen.
    projection: Projection,
}

impl VectorTranslator {
    /// Rakentaa kääntäjän eksplisiittisellä projektiolla.
    #[must_use]
    pub fn new(
        sender_model: impl Into<String>,
        sender_dims: usize,
        projection: Projection,
    ) -> Self {
        Self {
            sender_model: sender_model.into(),
            sender_dims,
            projection,
        }
    }

    /// Identiteettikääntäjä: arvoja ei muuteta, vain dimensio sovitetaan
    /// vastaanottajan kokoon. Sama-malli-käännös on tällöin häviötön.
    #[must_use]
    pub fn identity(sender_model: impl Into<String>, sender_dims: usize) -> Self {
        Self::new(sender_model, sender_dims, Projection::Identity)
    }

    /// Komponenttikohtainen affiininen kääntäjä
    /// (`y[i] = scale[i] * x[i] + offset[i]`).
    #[must_use]
    pub fn scale_offset(
        sender_model: impl Into<String>,
        sender_dims: usize,
        scale: Vec<f32>,
        offset: Vec<f32>,
    ) -> Self {
        Self::new(
            sender_model,
            sender_dims,
            Projection::ScaleOffset { scale, offset },
        )
    }

    /// Matriisikääntäjä `y = M x` (`rows` riviä, `cols` lähdesaraketta).
    #[must_use]
    pub fn matrix(
        sender_model: impl Into<String>,
        sender_dims: usize,
        rows: Vec<Vec<f32>>,
        cols: usize,
    ) -> Self {
        Self::new(sender_model, sender_dims, Projection::Matrix { rows, cols })
    }

    /// Lähettäjän mallitunniste.
    #[must_use]
    pub fn sender_model(&self) -> &str {
        &self.sender_model
    }

    /// Lähettäjän lähtödimensio.
    #[must_use]
    pub fn sender_dims(&self) -> usize {
        self.sender_dims
    }

    /// Onko tämä identiteettikartta sama-malli-käännökselle.
    #[must_use]
    pub fn is_identity_to(&self, to: &ReceiverProfile) -> bool {
        matches!(self.projection, Projection::Identity)
            && self.sender_model == to.model_id
            && self.sender_dims == to.dims
    }

    /// Kääntää vektorin lähettäjän avaruudesta vastaanottajan avaruuteen.
    ///
    /// Vaiheet:
    /// 1. Sovita lähde **lineaarisen kartan sisäänmenokokoon** deterministisesti
    ///    (pad nollilla / truncate). Truncate on häviöllinen.
    /// 2. Sovella lineaarinen kartta (identiteetti / scale-offset / matriisi).
    /// 3. Sovita tulos vastaanottajan odottamaan kokoon (`to.dims`)
    ///    deterministisesti (pad/truncate). Truncate on häviöllinen.
    ///
    /// Palauttaa **aina** [`ProjectedLatent`]:n — ei koskaan virhettä — jotta
    /// se yhtyy craten "viestintä ei katkea" -periaatteeseen. Häviöllisyys
    /// (`lossless == false`) on signaali kutsujalle harkita teksti-fallbackia;
    /// syy on luettavissa [`Self::fallback_reason`]-apurilla.
    ///
    /// Ei-äärellinen syöte (`NaN`/`inf`) käsitellään häviöllisenä ja arvot
    /// puhdistetaan nolliksi, jottei myrkky leviä vastaanottajan avaruuteen.
    #[must_use]
    pub fn translate(&self, v: &LatentVector, to: &ReceiverProfile) -> ProjectedLatent {
        let source_dims = v.dims.len();
        let target_dims = to.dims;

        // Puhdista ei-äärelliset arvot; merkitse häviölliseksi jos jouduttiin.
        let mut lossy = false;
        let cleaned: Vec<f32> = v
            .dims
            .iter()
            .map(|&x| {
                if x.is_finite() {
                    x
                } else {
                    lossy = true;
                    0.0
                }
            })
            .collect();

        // Vaihe 1: sovita kartan sisäänmenokokoon (jos kartta vaatii kiinteän).
        let map_input = self.projection.input_dims();
        let (fitted, fit_lossy) = match map_input {
            Some(n) => fit_to(&cleaned, n),
            None => (cleaned, false),
        };
        lossy |= fit_lossy;

        // Vaihe 2: sovella lineaarinen kartta.
        let mapped = self.apply_map(&fitted);

        // Vaihe 3: sovita vastaanottajan odottamaan kokoon.
        let (final_dims, final_lossy) = fit_to(&mapped, target_dims);
        lossy |= final_lossy;

        let strategy = self.strategy_for(source_dims, target_dims, to);

        ProjectedLatent {
            vector: LatentVector::new(final_dims, to.model_id.clone()),
            strategy,
            source_dims,
            target_dims,
            lossless: !lossy,
        }
    }

    /// Diagnostinen syy, jos [`translate`](Self::translate) joutui tekemään
    /// häviöllisen käännöksen. `None` jos tulos on häviötön.
    ///
    /// Tarkoitettu kutsujalle, joka haluaa kirjata
    /// [`FallbackReason::ProjectionFailed`]:n latent-mittausta varten.
    #[must_use]
    pub fn fallback_reason(projected: &ProjectedLatent) -> Option<FallbackReason> {
        if projected.lossless {
            None
        } else {
            Some(FallbackReason::ProjectionFailed)
        }
    }

    /// Soveltaa konfiguroidun lineaarisen kartan jo oikeankokoiseen syötteeseen.
    fn apply_map(&self, x: &[f32]) -> Vec<f32> {
        match &self.projection {
            Projection::Identity => x.to_vec(),
            Projection::ScaleOffset { scale, offset } => x
                .iter()
                .enumerate()
                .map(|(i, &xi)| {
                    let s = scale.get(i).copied().unwrap_or(1.0);
                    let o = offset.get(i).copied().unwrap_or(0.0);
                    s * xi + o
                })
                .collect(),
            Projection::Matrix { rows, .. } => rows
                .iter()
                .map(|row| {
                    row.iter()
                        .zip(x.iter())
                        .map(|(&m, &xi)| m * xi)
                        .sum::<f32>()
                })
                .collect(),
        }
    }

    /// Päättää raportoitavan [`ProjectionStrategy`]:n läpinäkyvyyttä varten.
    fn strategy_for(
        &self,
        source_dims: usize,
        target_dims: usize,
        to: &ReceiverProfile,
    ) -> ProjectionStrategy {
        // Eksplisiittinen kartta = aina Resize-tasoinen muunnos (arvoja muutettu).
        if !matches!(self.projection, Projection::Identity) {
            return ProjectionStrategy::Resize;
        }
        match target_dims.cmp(&source_dims) {
            std::cmp::Ordering::Greater => ProjectionStrategy::Pad,
            std::cmp::Ordering::Less => ProjectionStrategy::Truncate,
            std::cmp::Ordering::Equal => {
                if self.sender_model == to.model_id {
                    ProjectionStrategy::Identity
                } else {
                    ProjectionStrategy::Resize
                }
            }
        }
    }
}

/// Sovittaa komponenttijonon kohdepituuteen deterministisesti.
///
/// - Yhtä suuri → muuttumaton, häviötön.
/// - Lyhyempi kohde → truncate (häviöllinen).
/// - Pidempi kohde → pad nollilla (häviötön).
///
/// Palauttaa `(dims, lossy)`.
fn fit_to(src: &[f32], target: usize) -> (Vec<f32>, bool) {
    match target.cmp(&src.len()) {
        std::cmp::Ordering::Equal => (src.to_vec(), false),
        std::cmp::Ordering::Less => (src[..target].to_vec(), true),
        std::cmp::Ordering::Greater => {
            let mut out = Vec::with_capacity(target);
            out.extend_from_slice(src);
            out.resize(target, 0.0);
            (out, false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::cosine;

    fn vec_a(dims: Vec<f32>) -> LatentVector {
        LatentVector::new(dims, "agent_a/v1")
    }

    #[test]
    fn identity_same_model_preserves_vector() {
        let tr = VectorTranslator::identity("agent_a/v1", 3);
        let v = vec_a(vec![1.0, 2.0, 3.0]);
        let rx = ReceiverProfile::latent("agent_a/v1", 3);
        let p = tr.translate(&v, &rx);
        assert_eq!(p.vector.dims, vec![1.0, 2.0, 3.0]);
        assert_eq!(p.vector.model_id, "agent_a/v1");
        assert_eq!(p.strategy, ProjectionStrategy::Identity);
        assert!(p.lossless);
        assert!(VectorTranslator::fallback_reason(&p).is_none());
    }

    #[test]
    fn identity_translation_cosine_is_one() {
        let tr = VectorTranslator::identity("agent_a/v1", 4);
        let v = vec_a(vec![0.5, -1.0, 2.0, 3.5]);
        let rx = ReceiverProfile::latent("agent_a/v1", 4);
        let p = tr.translate(&v, &rx);
        assert!((cosine(&v.dims, &p.vector.dims) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn dim_mismatch_pad_is_handled_without_panic() {
        // Lähde 2-ulotteinen, vastaanottaja 4-ulotteinen → pad, häviötön.
        let tr = VectorTranslator::identity("agent_a/v1", 2);
        let v = vec_a(vec![7.0, 8.0]);
        let rx = ReceiverProfile::latent("agent_b/v1", 4);
        let p = tr.translate(&v, &rx);
        assert_eq!(p.vector.dims, vec![7.0, 8.0, 0.0, 0.0]);
        assert_eq!(p.strategy, ProjectionStrategy::Pad);
        assert_eq!(p.target_dims, 4);
        assert_eq!(p.source_dims, 2);
        assert!(p.lossless);
    }

    #[test]
    fn dim_mismatch_truncate_marks_lossy() {
        // Lähde 4-ulotteinen, vastaanottaja 2-ulotteinen → truncate, häviöllinen.
        let tr = VectorTranslator::identity("agent_a/v1", 4);
        let v = vec_a(vec![1.0, 2.0, 3.0, 4.0]);
        let rx = ReceiverProfile::latent("agent_b/v1", 2);
        let p = tr.translate(&v, &rx);
        assert_eq!(p.vector.dims, vec![1.0, 2.0]);
        assert_eq!(p.strategy, ProjectionStrategy::Truncate);
        assert!(!p.lossless);
        assert_eq!(
            VectorTranslator::fallback_reason(&p),
            Some(FallbackReason::ProjectionFailed)
        );
    }

    #[test]
    fn scale_offset_applies_affine_map() {
        // y[i] = 2*x[i] + 1
        let tr = VectorTranslator::scale_offset(
            "agent_a/v1",
            3,
            vec![2.0, 2.0, 2.0],
            vec![1.0, 1.0, 1.0],
        );
        let v = vec_a(vec![0.0, 1.0, 2.0]);
        let rx = ReceiverProfile::latent("agent_b/v1", 3);
        let p = tr.translate(&v, &rx);
        assert_eq!(p.vector.dims, vec![1.0, 3.0, 5.0]);
        assert_eq!(p.vector.model_id, "agent_b/v1");
        assert_eq!(p.strategy, ProjectionStrategy::Resize);
    }

    #[test]
    fn matrix_projection_maps_to_new_space() {
        // 3 → 2 matriisi: rivi0 = summa kahdesta ekasta, rivi1 = kolmas.
        let tr = VectorTranslator::matrix(
            "agent_a/v1",
            3,
            vec![vec![1.0, 1.0, 0.0], vec![0.0, 0.0, 1.0]],
            3,
        );
        let v = vec_a(vec![2.0, 3.0, 9.0]);
        let rx = ReceiverProfile::latent("agent_b/v1", 2);
        let p = tr.translate(&v, &rx);
        assert_eq!(p.vector.dims, vec![5.0, 9.0]);
        assert_eq!(p.target_dims, 2);
    }

    #[test]
    fn matrix_input_padding_when_source_smaller() {
        // Matriisi odottaa 3 saraketta, lähde on 2 → pad nollalla (häviötön),
        // sitten kartta. Rivi summaa kaikki kolme.
        let tr = VectorTranslator::matrix("agent_a/v1", 2, vec![vec![1.0, 1.0, 1.0]], 3);
        let v = vec_a(vec![4.0, 5.0]);
        let rx = ReceiverProfile::latent("agent_b/v1", 1);
        let p = tr.translate(&v, &rx);
        // (4 + 5 + 0) = 9
        assert_eq!(p.vector.dims, vec![9.0]);
        // Sisäänmenon pad on häviötön ja lopputulos on oikean kokoinen.
        assert!(p.lossless);
    }

    #[test]
    fn non_finite_input_is_sanitized_and_marked_lossy() {
        let tr = VectorTranslator::identity("agent_a/v1", 3);
        let v = LatentVector::new(vec![1.0, f32::NAN, f32::INFINITY], "agent_a/v1");
        let rx = ReceiverProfile::latent("agent_a/v1", 3);
        let p = tr.translate(&v, &rx);
        assert_eq!(p.vector.dims, vec![1.0, 0.0, 0.0]);
        assert!(p.vector.is_finite());
        assert!(!p.lossless);
    }

    #[test]
    fn empty_vector_translates_without_panic() {
        let tr = VectorTranslator::identity("agent_a/v1", 0);
        let v = vec_a(vec![]);
        let rx = ReceiverProfile::latent("agent_b/v1", 0);
        let p = tr.translate(&v, &rx);
        assert!(p.vector.is_empty());
        assert!(p.lossless);
    }

    #[test]
    fn accessors_report_translator_shape() {
        let tr = VectorTranslator::identity("agent_a/v1", 5);
        assert_eq!(tr.sender_model(), "agent_a/v1");
        assert_eq!(tr.sender_dims(), 5);
        let rx_same = ReceiverProfile::latent("agent_a/v1", 5);
        let rx_diff = ReceiverProfile::latent("agent_b/v1", 5);
        assert!(tr.is_identity_to(&rx_same));
        assert!(!tr.is_identity_to(&rx_diff));
    }

    #[test]
    fn translator_serde_roundtrip() {
        let tr = VectorTranslator::scale_offset("agent_a/v1", 2, vec![1.5, 0.5], vec![0.0, 1.0]);
        let json = serde_json::to_string(&tr).expect("serialize");
        let back: VectorTranslator = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(tr, back);
    }
}
