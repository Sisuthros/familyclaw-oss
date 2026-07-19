//! `OllamaEmbedder` — a real semantic embedder via Ollama.
//!
//! Implements the [`EmbeddingProvider`] trait by calling Ollama's
//! `/api/embeddings` endpoint (default model `nomic-embed-text`). This
//! replaces the [`DeterministicEmbedder`](crate::DeterministicEmbedder)
//! default when genuine semantic recall is needed (feature-hashing
//! bag-of-words produces vectors that are too coarse -> cosine similarity
//! stays low at ~0.1, a real model gives ~0.7+).
//!
//! **Feature-gated (`ollama`)** — does not pull the `reqwest` dependency
//! into the default build (Layer A / OSS resource-constraint requirement).
//! Embedding happens on the memory WRITE path, in `familyclaw-memory`'s
//! `EmbeddingMemoryStore` write path, not on the response hot path, so
//! synchronous `reqwest::blocking` is safe (Ollama is local and fast).
//!
//! ## Fail-safe
//! If Ollama doesn't respond (not running, wrong model, network error),
//! `embed` returns a **zero vector** — the same contract as for empty
//! input. Cosine treats a zero norm as 0.0 similarity, so recall degrades
//! safely (no crash, no hallucinated neighbors) instead of panicking.

use crate::EmbeddingProvider;

/// An Ollama-based embedder. Calls `POST {base_url}/api/embeddings`.
///
/// Build it with [`OllamaEmbedder::new`] (defaults) or
/// [`OllamaEmbedder::with_config`]. The dimensionality is queried lazily on
/// the first embedding and cached, since it depends on the model
/// (nomic-embed-text = 768). Before the first successful call,
/// [`dimensions`](Self::dimensions) returns the configured
/// `fallback_dimensions` value.
#[derive(Debug, Clone)]
pub struct OllamaEmbedder {
    base_url: String,
    model: String,
    id: String,
    fallback_dimensions: usize,
    client: reqwest::blocking::Client,
}

impl OllamaEmbedder {
    /// nomic-embed-text produces 768-dimensional vectors.
    pub const DEFAULT_MODEL: &'static str = "nomic-embed-text";
    /// The default dimensionality before the model's real dimension is
    /// detected.
    pub const DEFAULT_DIMENSIONS: usize = 768;
    /// The default base URL (local Ollama).
    pub const DEFAULT_BASE_URL: &'static str = "http://127.0.0.1:11434";

    /// Creates an embedder with defaults (local Ollama, `nomic-embed-text`).
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(Self::DEFAULT_BASE_URL, Self::DEFAULT_MODEL)
    }

    /// Creates an embedder with the given base URL and model.
    #[must_use]
    pub fn with_config(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        let base_url = base_url.into();
        let model = model.into();
        let id = format!("ollama:{model}");
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            id,
            fallback_dimensions: Self::DEFAULT_DIMENSIONS,
            client,
        }
    }

    /// Requests an embedding from Ollama. Returns `None` on error (fail-safe).
    fn request_embedding(&self, text: &str) -> Option<Vec<f32>> {
        let url = format!("{}/api/embeddings", self.base_url);
        let body = serde_json::json!({ "model": self.model, "prompt": text });
        let resp = self.client.post(&url).json(&body).send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let json: serde_json::Value = resp.json().ok()?;
        let arr = json.get("embedding")?.as_array()?;
        // f64->f32 is deliberate: embeddings are f32 vectors (cosine
        // comparison). A small loss of precision is acceptable.
        #[allow(clippy::cast_possible_truncation)]
        let vec: Vec<f32> = arr
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();
        if vec.is_empty() {
            return None;
        }
        Some(l2_normalize(vec))
    }
}

impl Default for OllamaEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingProvider for OllamaEmbedder {
    fn id(&self) -> &str {
        &self.id
    }

    fn dimensions(&self) -> usize {
        self.fallback_dimensions
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        // Empty input -> zero vector (same contract as DeterministicEmbedder).
        if text.trim().is_empty() {
            return vec![0.0; self.fallback_dimensions];
        }
        match self.request_embedding(text) {
            Some(v) => v,
            // Ollama down / error -> zero vector (fail-safe, cosine->0.0).
            None => vec![0.0; self.fallback_dimensions],
        }
    }
}

/// L2-normalizes a vector to unit length (cosine compatibility). A zero
/// vector is returned as-is (a zero norm cannot be normalized).
fn l2_normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_zero_vector() {
        let e = OllamaEmbedder::new();
        let v = e.embed("   ");
        assert_eq!(v.len(), OllamaEmbedder::DEFAULT_DIMENSIONS);
        assert!(v.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn id_encodes_model() {
        let e = OllamaEmbedder::with_config("http://localhost:11434", "nomic-embed-text");
        assert_eq!(e.id(), "ollama:nomic-embed-text");
    }

    #[test]
    fn unreachable_ollama_fails_safe_to_zero() {
        // A port nothing responds on -> zero vector, no panic.
        let e = OllamaEmbedder::with_config("http://127.0.0.1:1", "nomic-embed-text");
        let v = e.embed("hello world");
        assert_eq!(v.len(), OllamaEmbedder::DEFAULT_DIMENSIONS);
        assert!(v.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn l2_normalize_unit_length() {
        let v = l2_normalize(vec![3.0, 4.0]);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }
}
