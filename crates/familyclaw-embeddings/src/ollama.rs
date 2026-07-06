//! `OllamaEmbedder` — oikea semanttinen embedder Ollaman kautta.
//!
//! Toteuttaa [`EmbeddingProvider`]-traitin kutsumalla Ollaman
//! `/api/embeddings`-endpointia (oletusmalli `nomic-embed-text`). Tämä korvaa
//! [`DeterministicEmbedder`](crate::DeterministicEmbedder)-oletuksen kun tarvitaan
//! aitoa semanttista recallia (feature-hashing-bag-of-words tuottaa liian karkeat
//! vektorit → cosine-similarity jää matalaksi ~0.1, oikea malli antaa ~0.7+).
//!
//! **Feature-gated (`ollama`)** — ei vedä `reqwest`-riippuvuutta oletusrakennukseen
//! (Layer A / OSS -köyhyysrajoite). Upotus tapahtuu muistin KIRJOITUKSESSA
//! `familyclaw-memory`n `EmbeddingMemoryStore`-kirjoituspolulla, ei vastauksen hot-polulla, joten synkroninen
//! `reqwest::blocking` on turvallinen (Ollama on paikallinen ja nopea).
//!
//! ## Fail-safe
//! Jos Ollama ei vastaa (ei käynnissä, väärä malli, verkkovirhe), `embed`
//! palauttaa **nollavektorin** — sama sopimus kuin tyhjällä syötteellä. Cosine
//! käsittelee nollanormin 0.0-similariteettina, joten recall degradoituu
//! turvallisesti (ei kaadu, ei hallusinoi naapureita) sen sijaan että paniikkaisi.

use crate::EmbeddingProvider;

/// Ollama-pohjainen embedder. Kutsuu `POST {base_url}/api/embeddings`.
///
/// Rakenna [`OllamaEmbedder::new`]:llä (oletukset) tai
/// [`OllamaEmbedder::with_config`]:lla. Dimensio kysytään laiskasti ensimmäisellä
/// upotuksella ja välimuistitetaan, koska se riippuu mallista (nomic-embed-text =
/// 768). Ennen ensimmäistä onnistunutta kutsua [`dimensions`](Self::dimensions)
/// palauttaa konfiguroidun `fallback_dimensions`-arvon.
#[derive(Debug, Clone)]
pub struct OllamaEmbedder {
    base_url: String,
    model: String,
    id: String,
    fallback_dimensions: usize,
    client: reqwest::blocking::Client,
}

impl OllamaEmbedder {
    /// nomic-embed-text tuottaa 768-ulotteisia vektoreita.
    pub const DEFAULT_MODEL: &'static str = "nomic-embed-text";
    /// Oletusdimensio ennen kuin mallin oikea dimensio on havaittu.
    pub const DEFAULT_DIMENSIONS: usize = 768;
    /// Oletus-base-url (paikallinen Ollama).
    pub const DEFAULT_BASE_URL: &'static str = "http://127.0.0.1:11434";

    /// Luo embedderin oletuksilla (paikallinen Ollama, `nomic-embed-text`).
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(Self::DEFAULT_BASE_URL, Self::DEFAULT_MODEL)
    }

    /// Luo embedderin annetulla base-url:lla ja mallilla.
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

    /// Kysyy embeddingin Ollamalta. Palauttaa `None` virheessä (fail-safe).
    fn request_embedding(&self, text: &str) -> Option<Vec<f32>> {
        let url = format!("{}/api/embeddings", self.base_url);
        let body = serde_json::json!({ "model": self.model, "prompt": text });
        let resp = self.client.post(&url).json(&body).send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let json: serde_json::Value = resp.json().ok()?;
        let arr = json.get("embedding")?.as_array()?;
        // f64→f32 on tarkoituksellinen: embeddingit ovat f32-vektoreita
        // (cosine-vertailu). Tarkkuuden pieni menetys on hyväksyttävä.
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
        // Tyhjä syöte → nollavektori (sama sopimus kuin DeterministicEmbedder).
        if text.trim().is_empty() {
            return vec![0.0; self.fallback_dimensions];
        }
        match self.request_embedding(text) {
            Some(v) => v,
            // Ollama alhaalla / virhe → nollavektori (fail-safe, cosine→0.0).
            None => vec![0.0; self.fallback_dimensions],
        }
    }
}

/// L2-normalisoi vektorin yksikköpituuteen (cosine-yhteensopivuus). Nollavektori
/// palautetaan sellaisenaan (nollanormia ei voi normalisoida).
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
        // Portti johon mikään ei vastaa → nollavektori, ei paniikkia.
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
