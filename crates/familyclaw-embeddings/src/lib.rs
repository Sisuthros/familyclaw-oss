//! Embedding providers for the agent platform (Layer A, OSS).
//!
//! This crate gives text a vector representation that
//! [`familyclaw-memory`](../familyclaw_memory/index.html)'s vector search
//! (cosine similarity) can use. The design follows v1.0 roadmap decision
//! **D4**:
//!
//! - **Default = deterministic, dependency-free** ([`DeterministicEmbedder`]).
//!   No network, no model file, no native linking — respects the resource-
//!   constraint requirement and stays MSVC/`cargo deny`-green with no extra
//!   work. Produces *stable* vectors (same text → same vector, always), so
//!   replay and tests are deterministic.
//! - **Real (pure-Rust) models arrive behind feature gates** in a later PR
//!   (the backend chosen by the Phase 0 spike). The default build does not
//!   pull in a heavy dependency.
//!
//! ## What the deterministic default is NOT
//! [`DeterministicEmbedder`] is a **feature-hashing-based bag-of-words**
//! representation, not a learned semantic model. It captures *word overlap*
//! (shared tokens → higher cosine), not deep meaning. It is a deliberate
//! baseline: better than no vector, deterministic, and lets vector search
//! be built before a real model is available. Don't advertise it as
//! semantic search — semantic quality is measured with a recall benchmark
//! before a real model is wired in (roadmap D4).
//!
//! ## Example
//! ```
//! use familyclaw_embeddings::{DeterministicEmbedder, EmbeddingProvider};
//!
//! let embedder = DeterministicEmbedder::new();
//! let a = embedder.embed("kissa istuu matolla");
//! let b = embedder.embed("kissa istuu matolla");
//! assert_eq!(a, b); // deterministic: same text -> same vector
//! assert_eq!(a.len(), embedder.dimensions());
//! ```
//!
//! ## A real semantic embedder (feature `ollama`)
//! When real semantic recall is needed (not bag-of-words), enable the
//! `ollama` feature and use `OllamaEmbedder` (e.g. `nomic-embed-text`). It
//! calls a local Ollama instance and fail-safe-degrades to a zero vector if
//! Ollama does not respond.

#[cfg(feature = "ollama")]
pub mod ollama;
#[cfg(feature = "ollama")]
pub use ollama::OllamaEmbedder;

/// A text-to-vector provider.
///
/// Implementers produce a fixed-dimensional `f32` vector for a given text.
/// The same provider must produce **the same vector for the same text**
/// (stability is required for determinism and replay). Vectors are intended
/// for cosine-similarity comparison, so they are returned **L2-normalized**
/// (unit length) unless a provider documents otherwise.
pub trait EmbeddingProvider {
    /// A stable identifier for this provider (e.g. `"deterministic-hash-v1"`).
    ///
    /// Used for reporting (`status`/`doctor`) and to make sure vectors
    /// produced by different providers are never compared against each
    /// other.
    fn id(&self) -> &str;

    /// The dimensionality (length) of produced vectors. Fixed per provider.
    fn dimensions(&self) -> usize;

    /// Embeds text into a fixed-dimensional, L2-normalized vector.
    ///
    /// Empty input, or input containing only separators, returns a zero
    /// vector (all zeros) — the caller (cosine) must handle a zero norm
    /// (returning a 0.0 similarity), which is correct: empty text has no
    /// semantic neighbor.
    fn embed(&self, text: &str) -> Vec<f32>;
}

/// A deterministic, dependency-free default embedder (feature-hashing /
/// "hashing trick" bag-of-words).
///
/// Splits the text into tokens (ASCII lowercasing + non-alphanumeric
/// characters as separators), hashes each token with a deterministic
/// FNV-1a into two things: a **bucket index** (`% dimensions`) and a
/// **sign** (reduces hash-collision bias), accumulates the counters, and
/// L2-normalizes. Same text -> same vector every time, with no external
/// state.
///
/// The dimensionality defaults to
/// [`DeterministicEmbedder::DEFAULT_DIMENSIONS`].
#[derive(Debug, Clone)]
pub struct DeterministicEmbedder {
    dimensions: usize,
}

impl DeterministicEmbedder {
    /// The default dimensionality. 256 is a balance: wide enough to reduce
    /// collisions for typical messages, small enough for memory.
    pub const DEFAULT_DIMENSIONS: usize = 256;

    /// A stable identifier for this provider.
    pub const ID: &'static str = "deterministic-hash-v1";

    /// Creates the default embedder with
    /// [`Self::DEFAULT_DIMENSIONS`] dimensions.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            dimensions: Self::DEFAULT_DIMENSIONS,
        }
    }

    /// Creates an embedder with the given dimensionality.
    ///
    /// The dimensionality is clamped to at least 1 (a 0-dimensional vector
    /// is not useful and is not cosine-compatible).
    #[must_use]
    pub const fn with_dimensions(dimensions: usize) -> Self {
        Self {
            dimensions: if dimensions == 0 { 1 } else { dimensions },
        }
    }
}

impl Default for DeterministicEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

/// FNV-1a 64-bit hash (deterministic, dependency-free). A fixed seed, so
/// the same byte sequence -> the same hash on every run and machine.
fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

impl EmbeddingProvider for DeterministicEmbedder {
    fn id(&self) -> &str {
        Self::ID
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0.0f32; self.dimensions];

        // Tokenization: ASCII lowercasing, non-alphanumeric characters as
        // separators. Keep tokens simple and deterministic (no locale-
        // dependent unicode folding, which could vary across environments).
        let lower = text.to_ascii_lowercase();
        for token in lower.split(|c: char| !c.is_alphanumeric()) {
            if token.is_empty() {
                continue;
            }
            let h = fnv1a(token.as_bytes());
            // Low bits for the bucket index, one high bit for the sign.
            #[allow(clippy::cast_possible_truncation)]
            let bucket = (h % self.dimensions as u64) as usize;
            let sign = if (h >> 63) & 1 == 1 { -1.0 } else { 1.0 };
            vec[bucket] += sign;
        }

        l2_normalize(&mut vec);
        vec
    }
}

/// Normalizes a vector to unit length (L2) in place. A zero vector is left
/// as zero (no division by zero) — cosine treats it as 0.0.
fn l2_normalize(vec: &mut [f32]) {
    let norm_sq: f32 = vec.iter().map(|x| x * x).sum();
    if norm_sq <= 0.0 {
        return;
    }
    let norm = norm_sq.sqrt();
    for x in vec.iter_mut() {
        *x /= norm;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        dot // both are L2-normalized -> dot product = cosine
    }

    #[test]
    fn deterministic_same_text_same_vector() {
        let e = DeterministicEmbedder::new();
        assert_eq!(
            e.embed("kissa istuu matolla"),
            e.embed("kissa istuu matolla")
        );
    }

    #[test]
    fn dimensions_match_config() {
        let e = DeterministicEmbedder::new();
        assert_eq!(e.embed("mitä tahansa").len(), e.dimensions());
        assert_eq!(e.dimensions(), DeterministicEmbedder::DEFAULT_DIMENSIONS);

        let e8 = DeterministicEmbedder::with_dimensions(8);
        assert_eq!(e8.embed("hei").len(), 8);
    }

    #[test]
    fn zero_dimensions_is_clamped_to_one() {
        let e = DeterministicEmbedder::with_dimensions(0);
        assert_eq!(e.dimensions(), 1);
        assert_eq!(e.embed("x").len(), 1);
    }

    #[test]
    fn empty_text_is_zero_vector() {
        let e = DeterministicEmbedder::new();
        let v = e.embed("");
        assert_eq!(v.len(), e.dimensions());
        assert!(v.iter().all(|&x| x == 0.0), "empty text -> zero vector");
        // Only separators -> also a zero vector.
        assert!(e.embed("   !!! ").iter().all(|&x| x == 0.0));
    }

    #[test]
    fn non_empty_vector_is_unit_length() {
        let e = DeterministicEmbedder::new();
        let v = e.embed("kissa koira hevonen");
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "L2 norm ~1, was {norm}");
    }

    #[test]
    fn shared_words_score_higher_than_disjoint() {
        let e = DeterministicEmbedder::new();
        let base = e.embed("kissa istuu matolla");
        let overlap = e.embed("kissa istuu tuolilla"); // 2/3 shared
        let disjoint = e.embed("auto ajaa moottoritiellä"); // no overlap
        let sim_overlap = cosine(&base, &overlap);
        let sim_disjoint = cosine(&base, &disjoint);
        assert!(
            sim_overlap > sim_disjoint,
            "shared words -> higher cosine: overlap={sim_overlap}, disjoint={sim_disjoint}"
        );
    }

    #[test]
    fn identical_text_has_cosine_one() {
        let e = DeterministicEmbedder::new();
        let v = e.embed("sama teksti molemmin puolin");
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn provider_id_is_stable() {
        let e = DeterministicEmbedder::new();
        assert_eq!(e.id(), "deterministic-hash-v1");
        assert_eq!(e.id(), DeterministicEmbedder::ID);
    }

    #[test]
    fn fnv1a_is_stable() {
        // Lock in hash stability: if this changes, all stored vectors
        // change -> a deliberate breaking change, not an accident.
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn trait_object_usable() {
        // Verify the trait is object-safe (dyn) — the runtime selects the
        // provider at runtime.
        let provider: Box<dyn EmbeddingProvider> = Box::new(DeterministicEmbedder::new());
        assert_eq!(provider.dimensions(), 256);
        assert_eq!(provider.embed("test").len(), 256);
    }
}
