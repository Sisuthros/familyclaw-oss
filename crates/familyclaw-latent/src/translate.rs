//! `VectorTranslator` — a cross-model latent-vector *translator* from the
//! sender model's space into the receiver model's space.
//!
//! ## How does this differ from [`crate::link::RecursiveLink`]?
//! `RecursiveLink` performs only **dimension fitting** (pad/truncate/resize):
//! it never touches the values. `VectorTranslator` adds a **configurable
//! linear projection** on top of that — either a per-component `scale`/`offset`
//! or a full `matrix` — that approximates the map between the two spaces.
//!
//! ## Important honesty boundary (no overselling)
//! This is an MVP: **a configurable linear projection, NOT a trained encoder.**
//! The Direct Semantic Communication paper (2025) uses a trained dual-encoder
//! translator (~0.538 cosine) and a 30% blend. Here the matrix/scale is
//! supplied externally — it provides *structure* and testability, but does
//! not claim to have learned semantic alignment. The identity projection
//! (the default) leaves the vector unchanged, so same-model translation is
//! lossless.
//!
//! Dimension mismatches are handled **deterministically** (truncate/pad before
//! the linear map), and lossiness is recorded via
//! [`FallbackReason::ProjectionFailed`] in the result.
//!
//! ## OSS boundary (Layer A)
//! No hardcoded model names — all model identifiers and coefficients are
//! supplied at runtime. Examples use generic names (`agent_a`, `agent_b`).

use serde::{Deserialize, Serialize};

use crate::channel::{FallbackReason, ReceiverProfile};
use crate::link::{ProjectedLatent, ProjectionStrategy};
use crate::vector::LatentVector;

/// A configurable linear map from the sender's space to the receiver's
/// space. MVP — supplied externally, not learned.
///
/// All transforms operate in the **receiver's dimensionality**: the source
/// is first fitted to the target size (deterministic pad/truncate) and then
/// the linear map is applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Projection {
    /// Identity: values are unchanged (only the dimension is fitted).
    Identity,
    /// Per-component affine map: `y[i] = scale[i] * x[i] + offset[i]`.
    /// The length of the `scale` and `offset` vectors determines the target
    /// dimension.
    ScaleOffset {
        /// Per-component scale factors.
        scale: Vec<f32>,
        /// Per-component offsets.
        offset: Vec<f32>,
    },
    /// Full matrix map `y = M x`, given row by row (`rows` x `cols`).
    /// `cols` is the source dimension (after fitting), `rows` is the target
    /// dimension.
    Matrix {
        /// Rows; each row has length `cols`.
        rows: Vec<Vec<f32>>,
        /// Number of columns (= the source dimension expected at the map's
        /// input).
        cols: usize,
    },
}

impl Projection {
    /// The source dimension this projection expects at its input.
    ///
    /// `None` = any size (identity / per-component map only requires the
    /// input to already be the target size).
    #[must_use]
    fn input_dims(&self) -> Option<usize> {
        match self {
            Self::Identity => None,
            Self::ScaleOffset { scale, .. } => Some(scale.len()),
            Self::Matrix { cols, .. } => Some(*cols),
        }
    }
}

/// A unit that translates from the sender model's space into the receiver's
/// space.
///
/// Encapsulates the sender's model identifier and its output dimension,
/// along with a [`Projection`] configuration.
/// [`translate`](VectorTranslator::translate) always produces a
/// [`ProjectedLatent`] — communication never breaks even if the dimensions
/// don't match (in that case a deterministic, lossy fit is performed and
/// flagged).
///
/// # Example
/// ```
/// use familyclaw_latent::translate::{Projection, VectorTranslator};
/// use familyclaw_latent::{LatentVector, ReceiverProfile};
///
/// // Same-model translation with identity preserves the vector.
/// let tr = VectorTranslator::identity("agent_a/v1", 3);
/// let v = LatentVector::new(vec![1.0, 2.0, 3.0], "agent_a/v1");
/// let rx = ReceiverProfile::latent("agent_a/v1", 3);
/// let projected = tr.translate(&v, &rx);
/// assert_eq!(projected.vector.dims, vec![1.0, 2.0, 3.0]);
/// assert!(projected.lossless);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorTranslator {
    /// The sender's model identifier (`"provider/model"`).
    sender_model: String,
    /// The sender's output dimension (the vector's expected size before
    /// translation).
    sender_dims: usize,
    /// The linear map from the sender's space to the receiver's space.
    projection: Projection,
}

impl VectorTranslator {
    /// Builds a translator with an explicit projection.
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

    /// Identity translator: values are unchanged, only the dimension is
    /// fitted to the receiver's size. Same-model translation is then
    /// lossless.
    #[must_use]
    pub fn identity(sender_model: impl Into<String>, sender_dims: usize) -> Self {
        Self::new(sender_model, sender_dims, Projection::Identity)
    }

    /// Per-component affine translator
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

    /// Matrix translator `y = M x` (`rows` rows, `cols` source columns).
    #[must_use]
    pub fn matrix(
        sender_model: impl Into<String>,
        sender_dims: usize,
        rows: Vec<Vec<f32>>,
        cols: usize,
    ) -> Self {
        Self::new(sender_model, sender_dims, Projection::Matrix { rows, cols })
    }

    /// The sender's model identifier.
    #[must_use]
    pub fn sender_model(&self) -> &str {
        &self.sender_model
    }

    /// The sender's output dimension.
    #[must_use]
    pub fn sender_dims(&self) -> usize {
        self.sender_dims
    }

    /// Whether this is an identity map for a same-model translation.
    #[must_use]
    pub fn is_identity_to(&self, to: &ReceiverProfile) -> bool {
        matches!(self.projection, Projection::Identity)
            && self.sender_model == to.model_id
            && self.sender_dims == to.dims
    }

    /// Translates a vector from the sender's space into the receiver's
    /// space.
    ///
    /// Steps:
    /// 1. Deterministically fit the source to the **linear map's input
    ///    size** (zero-pad / truncate). Truncation is lossy.
    /// 2. Apply the linear map (identity / scale-offset / matrix).
    /// 3. Deterministically fit the result to the receiver's expected size
    ///    (`to.dims`) (pad/truncate). Truncation is lossy.
    ///
    /// **Always** returns a [`ProjectedLatent`] — never an error — so it
    /// aligns with the crate's "communication never breaks" principle.
    /// Lossiness (`lossless == false`) is a signal for the caller to
    /// consider a text fallback; the reason can be read via
    /// [`Self::fallback_reason`].
    ///
    /// Non-finite input (`NaN`/`inf`) is treated as lossy and the values are
    /// sanitized to zero, so the poison doesn't spread into the receiver's
    /// space.
    #[must_use]
    pub fn translate(&self, v: &LatentVector, to: &ReceiverProfile) -> ProjectedLatent {
        let source_dims = v.dims.len();
        let target_dims = to.dims;

        // Sanitize non-finite values; mark as lossy if we had to.
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

        // Step 1: fit to the map's input size (if the map requires a fixed one).
        let map_input = self.projection.input_dims();
        let (fitted, fit_lossy) = match map_input {
            Some(n) => fit_to(&cleaned, n),
            None => (cleaned, false),
        };
        lossy |= fit_lossy;

        // Step 2: apply the linear map.
        let mapped = self.apply_map(&fitted);

        // Step 3: fit to the receiver's expected size.
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

    /// A diagnostic reason if [`translate`](Self::translate) had to perform
    /// a lossy translation. `None` if the result is lossless.
    ///
    /// Intended for a caller that wants to record
    /// [`FallbackReason::ProjectionFailed`] for latent-transfer metrics.
    #[must_use]
    pub fn fallback_reason(projected: &ProjectedLatent) -> Option<FallbackReason> {
        if projected.lossless {
            None
        } else {
            Some(FallbackReason::ProjectionFailed)
        }
    }

    /// Applies the configured linear map to input that is already the
    /// correct size.
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

    /// Decides the [`ProjectionStrategy`] to report, for transparency.
    fn strategy_for(
        &self,
        source_dims: usize,
        target_dims: usize,
        to: &ReceiverProfile,
    ) -> ProjectionStrategy {
        // An explicit map is always a Resize-level transform (values were changed).
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

/// Deterministically fits a component sequence to the target length.
///
/// - Equal length -> unchanged, lossless.
/// - Shorter target -> truncate (lossy).
/// - Longer target -> zero-pad (lossless).
///
/// Returns `(dims, lossy)`.
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
        // Source is 2-dimensional, receiver is 4-dimensional -> pad, lossless.
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
        // Source is 4-dimensional, receiver is 2-dimensional -> truncate, lossy.
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
        // 3 -> 2 matrix: row0 = sum of the first two, row1 = the third.
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
        // Matrix expects 3 columns, source is 2 -> zero-pad (lossless),
        // then the map. The row sums all three.
        let tr = VectorTranslator::matrix("agent_a/v1", 2, vec![vec![1.0, 1.0, 1.0]], 3);
        let v = vec_a(vec![4.0, 5.0]);
        let rx = ReceiverProfile::latent("agent_b/v1", 1);
        let p = tr.translate(&v, &rx);
        // (4 + 5 + 0) = 9
        assert_eq!(p.vector.dims, vec![9.0]);
        // The input pad is lossless and the final result is the right size.
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
