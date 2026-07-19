//! `RecursiveLink` — a dimension bridge from agent A's latent space into
//! agent B's latent space.
//!
//! Different models produce hidden states with different dimensionality
//! (e.g. agent A is 8-dimensional, agent B is 12-dimensional). For siblings
//! to exchange hidden states, the dimensions must be **bridged** together.
//!
//! ## Important limitation (honest skeleton, no overselling)
//! This implementation performs only a **simple linear fit**: truncation if
//! the source is larger, or zero-padding if the source is smaller. This
//! **keeps communication working**, but is not a semantically learned
//! projection — two different models' latent spaces are not aligned, so
//! plain pad/truncate **does not guarantee that meaning is preserved**. A
//! real learned projection matrix (e.g. a LatentMAS-style trained fit) will
//! arrive in a later iteration; until then [`crate::channel`] always
//! provides a text fallback.
//!
//! An **identity bridge** (same source and target model) is also
//! supported: in that case the vector is not modified at all, since it's
//! already in the right space.

use serde::{Deserialize, Serialize};

use crate::vector::LatentVector;

/// The projection strategy [`RecursiveLink`] uses to fit dimensions between
/// the source and target space.
///
/// The strategy is chosen automatically based on the source's and target's
/// dimensionality, but it is recorded in the result
/// ([`ProjectedLatent::strategy`]) for traceability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStrategy {
    /// The source and target model are the same: the vector is transferred
    /// unchanged.
    Identity,
    /// The target space is smaller: excess components are dropped.
    Truncate,
    /// The target space is larger: missing components are filled with
    /// zeros.
    Pad,
    /// The source and target dimensions are equal (but the models differ):
    /// components are copied as-is.
    Resize,
}

/// The result of a single projection: the [`LatentVector`] bridged into the
/// target space, plus metadata about how the bridge was done.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectedLatent {
    /// The latent vector fitted to the target model.
    pub vector: LatentVector,
    /// The projection strategy used.
    pub strategy: ProjectionStrategy,
    /// The source's original dimensionality (before projection).
    pub source_dims: usize,
    /// The target's dimensionality (after projection).
    pub target_dims: usize,
    /// `true` if the projection was **lossless** (identity or plain
    /// zero-padding). `false` if truncation dropped components.
    pub lossless: bool,
}

/// A linear dimension bridge between two models' latent spaces.
///
/// `RecursiveLink` describes that agent A's (`source_model`,
/// `source_dims`) hidden states can be transferred into agent B's
/// (`target_model`, `target_dims`) space. The name *recursive* refers to
/// the design document's RecursiveMAS/LatentMAS roots; this version is its
/// first, linear approximation.
///
/// # Example
/// ```
/// use familyclaw_latent::link::{RecursiveLink, ProjectionStrategy};
/// use familyclaw_latent::vector::LatentVector;
///
/// // agent_a produces a 4-dimensional state, agent_b expects 6-dimensional.
/// let link = RecursiveLink::new("agent_a/v1", 4, "agent_b/v1", 6);
/// let v = LatentVector::new(vec![1.0, 2.0, 3.0, 4.0], "agent_a/v1");
/// let projected = link.project(&v).expect("dimensions match the link");
/// assert_eq!(projected.vector.len(), 6);
/// assert_eq!(projected.strategy, ProjectionStrategy::Pad);
/// assert!(projected.lossless); // plain zero-padding drops nothing
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecursiveLink {
    /// The source model's identifier (`"provider/model"`).
    source_model: String,
    /// The source space's dimensionality.
    source_dims: usize,
    /// The target model's identifier (`"provider/model"`).
    target_model: String,
    /// The target space's dimensionality.
    target_dims: usize,
}

impl RecursiveLink {
    /// Builds a new dimension bridge from a source model to a target model.
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

    /// The source model's identifier.
    #[must_use]
    pub fn source_model(&self) -> &str {
        &self.source_model
    }

    /// The target model's identifier.
    #[must_use]
    pub fn target_model(&self) -> &str {
        &self.target_model
    }

    /// The source space's dimensionality.
    #[must_use]
    pub fn source_dims(&self) -> usize {
        self.source_dims
    }

    /// The target space's dimensionality.
    #[must_use]
    pub fn target_dims(&self) -> usize {
        self.target_dims
    }

    /// Whether this is an identity bridge (same source and target model
    /// **and** the same dimensionality).
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.source_model == self.target_model && self.source_dims == self.target_dims
    }

    /// Projects the given vector into the target space.
    ///
    /// Performs a simple linear fit:
    /// - same model + same size -> [`ProjectionStrategy::Identity`]
    /// - same size, different model -> [`ProjectionStrategy::Resize`] (copy)
    /// - target larger -> [`ProjectionStrategy::Pad`] (zero-padding, lossless)
    /// - target smaller -> [`ProjectionStrategy::Truncate`] (lossy)
    ///
    /// # Errors
    /// Returns [`crate::FamilyClawError::InvalidInput`] if:
    /// - the vector's `model_id` doesn't match the bridge's `source_model`, or
    /// - the vector's dimensionality doesn't match the bridge's
    ///   `source_dims`, or
    /// - the vector contains non-finite values (`NaN`/`inf`).
    ///
    /// The error is a deliberate signal for the caller to fall back to text.
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
                // Pad: copy the source + fill the rest with zeros. Lossless.
                let mut dims = Vec::with_capacity(target_dims);
                dims.extend_from_slice(&vector.dims);
                dims.resize(target_dims, 0.0);
                (dims, ProjectionStrategy::Pad, true)
            }
            std::cmp::Ordering::Less => {
                // Truncate: keep only the first `target_dims` components.
                // Lossy — part of the hidden state is discarded.
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
        // Edge case: empty source, empty target.
        let link = RecursiveLink::new("agent_a/v1", 0, "agent_b/v1", 0);
        let p = link.project(&vec_a(vec![])).expect("empty ok");
        assert!(p.vector.is_empty());
        assert_eq!(p.strategy, ProjectionStrategy::Resize);
    }

    #[test]
    fn pad_from_empty_source() {
        // Edge case: 0 -> N padding produces all zeros.
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
