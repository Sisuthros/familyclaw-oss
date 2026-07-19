//! Latent vectors — an agent's hidden-state representation.
//!
//! [`LatentVector`] encapsulates one agent's *hidden state*: the final
//! layer's activations as floating-point numbers, along with a record of
//! **which model** produced it ([`LatentVector::model_id`]). The model
//! identifier is critical because different models' latent spaces are not
//! directly comparable without a projection (see [`crate::link`]).
//!
//! ## Research context
//! This is an honest *skeleton* for LatentMAS-style (ICML 2026) sibling
//! communication. The vector itself is just a numeric carrier — it does not
//! claim to understand what the hidden state means. A real learned
//! interpretation/projection comes later; for now we keep the structure
//! simple and verifiable.

use serde::{Deserialize, Serialize};

/// An agent's hidden-state representation: a vector of floating-point
/// numbers plus the identifier of the model that produced it.
///
/// `model_id` is a free-form model identifier in the form `"provider/model"`
/// (e.g. `"agent-a-model/v1"`). It is used to check whether two vectors can
/// be combined directly or whether a [`crate::link::RecursiveLink`] is
/// needed.
///
/// # OSS boundary (Layer A)
/// This type never hardcodes real model names or family member data —
/// `model_id` is always supplied at runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatentVector {
    /// The hidden state's numeric components (the model's final layer
    /// activations, or an equivalent condensed representation).
    pub dims: Vec<f32>,
    /// The identifier of the model that produced this vector, in
    /// `"provider/model"` form.
    pub model_id: String,
}

impl LatentVector {
    /// Creates a new latent vector from the given components and model
    /// identifier.
    ///
    /// An empty `dims` is allowed (representing "no hidden state") —
    /// compatibility and fallback logic handle it safely.
    #[must_use]
    pub fn new(dims: Vec<f32>, model_id: impl Into<String>) -> Self {
        Self {
            dims,
            model_id: model_id.into(),
        }
    }

    /// The vector's dimensionality (number of components).
    #[must_use]
    pub fn len(&self) -> usize {
        self.dims.len()
    }

    /// Whether the vector is empty (no components at all).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dims.is_empty()
    }

    /// Whether the model that produced this vector matches the `other`
    /// model identifier.
    ///
    /// This is a fast pre-check: if the models are the same, the vectors
    /// live in the same latent space and no projection is needed.
    #[must_use]
    pub fn same_model(&self, other_model_id: &str) -> bool {
        self.model_id == other_model_id
    }

    /// Whether the vector is numerically sound: all components are finite
    /// (no `NaN`, no infinity).
    ///
    /// An unsound hidden state is a signal that the latent transfer should
    /// be rejected and fall back to text (see [`crate::channel`]).
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.dims.iter().all(|x| x.is_finite())
    }

    /// The L2 norm (Euclidean length). Useful for sanity checks and for
    /// measuring the effect of a projection.
    #[must_use]
    pub fn l2_norm(&self) -> f32 {
        self.dims.iter().map(|x| x * x).sum::<f32>().sqrt()
    }
}

/// Cosine similarity between two component sequences.
///
/// Returns a value in `[-1.0, 1.0]`, where `1.0` means identical direction
/// (used in tests to measure whether a projection/blend preserves the
/// direction of meaning). Measures **direction**, not length.
///
/// Edge cases (return `0.0`, never `NaN` — suitable for scoring):
/// - sequences of different length (not comparable),
/// - empty sequences,
/// - either sequence is a zero vector (direction undefined),
/// - non-finite values (`NaN`/`inf`) in the input.
///
/// # Example
/// ```
/// use familyclaw_latent::vector::cosine;
/// assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
/// assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6); // orthogonal
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

    // Numerically clamp to [-1, 1] (floating-point error can exceed the bound).
    (dot / denom).clamp(-1.0, 1.0)
}

/// Linearly blends two equal-length vectors by the given blend strength
/// (Direct Semantic Communication-style ~30% blend).
///
/// The result is, component-wise, `original * (1 - strength) + other * strength`.
/// `strength` is clamped to `[0.0, 1.0]`:
/// - `0.0` -> returns `original` unchanged,
/// - `1.0` -> returns `other` (as long as the lengths match),
/// - `0.3` -> the Direct Semantic paper's default blend.
///
/// The result's `model_id` is inherited from `original` — the blend lives in
/// the sender's space and does not claim to have switched models.
///
/// # Errors
/// Returns [`crate::FamilyClawError::InvalidInput`] if the vectors have
/// different lengths (blending requires aligned, equal-dimension components).
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
        // An empty vector is vacuously finite — all zero components are
        // finite.
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
        // Different length, empty, zero vector, non-finite -> 0.0 (never NaN).
        assert!(cosine(&[1.0, 2.0], &[1.0]).abs() < f32::EPSILON);
        assert!(cosine(&[], &[]).abs() < f32::EPSILON);
        assert!(cosine(&[0.0, 0.0], &[1.0, 1.0]).abs() < f32::EPSILON);
        assert!(cosine(&[f32::NAN, 1.0], &[1.0, 1.0]).abs() < f32::EPSILON);
        assert!(!cosine(&[0.0], &[0.0]).is_nan());
    }

    #[test]
    fn cosine_is_scale_invariant() {
        // Cosine measures direction, not length: scaling doesn't change the result.
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
        // model_id is inherited from the original.
        assert_eq!(blended.model_id, "agent_a/v1");
    }

    #[test]
    fn blend_at_one_returns_other_values() {
        let a = LatentVector::new(vec![1.0, 2.0], "agent_a/v1");
        let b = LatentVector::new(vec![9.0, 8.0], "agent_b/v1");
        let blended = blend(&a, &b, 1.0).expect("equal dims");
        assert_eq!(blended.dims, vec![9.0, 8.0]);
        // model_id still stays in the original's space.
        assert_eq!(blended.model_id, "agent_a/v1");
    }

    #[test]
    fn blend_at_thirty_percent_is_weighted_mix() {
        // Direct Semantic paper's default: 30% blend.
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
        // < 0 clamps to 0 (original), > 1 clamps to 1 (other).
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
