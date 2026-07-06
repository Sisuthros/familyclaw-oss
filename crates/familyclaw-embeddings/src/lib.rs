//! Embedding-tarjoajat agenttialustalle (KERROS A, OSS).
//!
//! Tämä crate antaa tekstille vektoriedustuksen, jota
//! [`familyclaw-memory`](../familyclaw_memory/index.html)n vektorihaku
//! (cosine-similarity) voi käyttää. Suunnittelu noudattaa v1.0-roadmapin
//! päätöstä **D4**:
//!
//! - **Oletus = deterministinen, riippuvuudeton** ([`DeterministicEmbedder`]).
//!   Ei verkkoa, ei mallitiedostoa, ei natiivilinkkiä — kunnioittaa köyhyys-
//!   rajoitetta ja pysyy MSVC/`cargo deny`-vihreänä ilman lisätyötä. Tuottaa
//!   *vakaat* vektorit (sama teksti → sama vektori, aina), joten replay ja
//!   testit ovat deterministisiä.
//! - **Aidot mallit (pure-Rust) tulevat feature-gatein** myöhemmässä PR:ssä
//!   (Phase-0-spiken valitsema backend). Oletusrakennus ei vedä raskasta
//!   riippuvuutta.
//!
//! ## Mitä deterministinen oletus EI ole
//! [`DeterministicEmbedder`] on **feature-hashing-pohjainen bag-of-words**
//! -edustus, ei opittu semanttinen malli. Se vangitsee *sanojen päällekkäisyyden*
//! (jaetut tokenit → korkeampi cosine), ei syvää merkitystä. Se on
//! tarkoituksellinen perustaso: parempi kuin ei vektoria, deterministinen, ja
//! antaa vektorihaun rakentua ennen kuin aito malli on saatavilla. Älä mainosta
//! sitä semanttisena hakuna — semanttinen laatu mitataan recall-benchmarkilla
//! ennen kuin aito malli kytketään (roadmap D4).
//!
//! ## Esimerkki
//! ```
//! use familyclaw_embeddings::{DeterministicEmbedder, EmbeddingProvider};
//!
//! let embedder = DeterministicEmbedder::new();
//! let a = embedder.embed("kissa istuu matolla");
//! let b = embedder.embed("kissa istuu matolla");
//! assert_eq!(a, b); // deterministinen: sama teksti → sama vektori
//! assert_eq!(a.len(), embedder.dimensions());
//! ```
//!
//! ## Aito semanttinen embedder (feature `ollama`)
//! Kun tarvitaan aitoa semanttista recallia (ei bag-of-words), ota käyttöön
//! `ollama`-feature ja käytä `OllamaEmbedder`:ia (esim. `nomic-embed-text`).
//! Se kutsuu paikallista Ollamaa ja fail-safe-degradoituu nollavektoriin jos
//! Ollama ei vastaa.

#[cfg(feature = "ollama")]
pub mod ollama;
#[cfg(feature = "ollama")]
pub use ollama::OllamaEmbedder;

/// Tekstistä vektoriksi -tarjoaja.
///
/// Toteuttajat tuottavat kiinteäulotteisen `f32`-vektorin annetulle tekstille.
/// Saman tarjoajan on tuotettava **sama vektori samalle tekstille** (vakaus on
/// välttämätön determinismille ja replaylle). Vektorit on tarkoitettu
/// cosine-similarity-vertailuun, joten ne palautetaan **L2-normalisoituina**
/// (yksikköpituus) ellei tarjoaja erikseen muuta dokumentoi.
pub trait EmbeddingProvider {
    /// Vakaa tunniste tälle tarjoajalle (esim. `"deterministic-hash-v1"`).
    ///
    /// Käytetään raportointiin (`status`/`doctor`) ja sen varmistamiseen, ettei
    /// eri tarjoajilla tuotettuja vektoreita verrata keskenään.
    fn id(&self) -> &str;

    /// Tuotettujen vektorien ulottuvuus (pituus). Kiinteä tarjoajaa kohden.
    fn dimensions(&self) -> usize;

    /// Upottaa tekstin kiinteäulotteiseksi, L2-normalisoiduksi vektoriksi.
    ///
    /// Tyhjä tai pelkkiä erottimia sisältävä syöte palauttaa nollavektorin
    /// (kaikki nollia) — kutsujan (cosine) on käsiteltävä nollanormi (palauttaa
    /// 0.0 similariteetin), mikä on oikein: tyhjällä tekstillä ei ole
    /// semanttista naapuria.
    fn embed(&self, text: &str) -> Vec<f32>;
}

/// Deterministinen, riippuvuudeton oletus-embedder (feature-hashing / "hashing
/// trick" -bag-of-words).
///
/// Pilkkoo tekstin tokeneiksi (ASCII-pienennys + ei-alfanumeeriset erottimina),
/// hashaa kunkin tokenin deterministisellä FNV-1a:lla kahteen asiaan:
/// **ämpäri-indeksiin** (`% dimensions`) ja **etumerkkiin** (vähentää
/// hash-kollisioiden harhaa), kasaa laskurit ja L2-normalisoi. Sama teksti →
/// sama vektori joka kerta, ilman ulkoista tilaa.
///
/// Ulottuvuus on oletuksena [`DeterministicEmbedder::DEFAULT_DIMENSIONS`].
#[derive(Debug, Clone)]
pub struct DeterministicEmbedder {
    dimensions: usize,
}

impl DeterministicEmbedder {
    /// Oletusulottuvuus. 256 on tasapaino: tarpeeksi väljä kollisioiden
    /// vähentämiseksi tavallisilla viesteillä, tarpeeksi pieni muistille.
    pub const DEFAULT_DIMENSIONS: usize = 256;

    /// Vakaa tunniste tälle tarjoajalle.
    pub const ID: &'static str = "deterministic-hash-v1";

    /// Luo oletus-embedderin [`Self::DEFAULT_DIMENSIONS`]-ulottuvuudella.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            dimensions: Self::DEFAULT_DIMENSIONS,
        }
    }

    /// Luo embedderin annetulla ulottuvuudella.
    ///
    /// Ulottuvuus pakotetaan vähintään 1:ksi (0-ulotteinen vektori ei ole
    /// hyödyllinen eikä cosine-yhteensopiva).
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

/// FNV-1a 64-bit -hash (deterministinen, riippuvuudeton). Vakio-siemen, joten
/// sama tavujono → sama hash joka ajossa ja koneessa.
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

        // Tokenisointi: ASCII-pienennys, ei-alfanumeeriset erottimina.
        // Pidetään tokenit yksinkertaisina ja deterministisinä (ei lokaali-
        // riippuvaista unicode-foldausta, joka voisi vaihdella ympäristöittäin).
        let lower = text.to_ascii_lowercase();
        for token in lower.split(|c: char| !c.is_alphanumeric()) {
            if token.is_empty() {
                continue;
            }
            let h = fnv1a(token.as_bytes());
            // Alabitit ämpäri-indeksiin, yksi ylempi bitti etumerkkiin.
            #[allow(clippy::cast_possible_truncation)]
            let bucket = (h % self.dimensions as u64) as usize;
            let sign = if (h >> 63) & 1 == 1 { -1.0 } else { 1.0 };
            vec[bucket] += sign;
        }

        l2_normalize(&mut vec);
        vec
    }
}

/// Normalisoi vektorin yksikköpituuteen (L2) paikallaan. Nollavektori jätetään
/// nollaksi (ei jakoa nollalla) — cosine käsittelee sen 0.0:na.
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
        dot // molemmat L2-normalisoituja → piste = cosine
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
        assert!(v.iter().all(|&x| x == 0.0), "tyhjä teksti → nollavektori");
        // Vain erottimet → myös nollavektori.
        assert!(e.embed("   !!! ").iter().all(|&x| x == 0.0));
    }

    #[test]
    fn non_empty_vector_is_unit_length() {
        let e = DeterministicEmbedder::new();
        let v = e.embed("kissa koira hevonen");
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "L2-normi ~1, oli {norm}");
    }

    #[test]
    fn shared_words_score_higher_than_disjoint() {
        let e = DeterministicEmbedder::new();
        let base = e.embed("kissa istuu matolla");
        let overlap = e.embed("kissa istuu tuolilla"); // 2/3 jaettua
        let disjoint = e.embed("auto ajaa moottoritiellä"); // ei jaettua
        let sim_overlap = cosine(&base, &overlap);
        let sim_disjoint = cosine(&base, &disjoint);
        assert!(
            sim_overlap > sim_disjoint,
            "jaetut sanat → korkeampi cosine: overlap={sim_overlap}, disjoint={sim_disjoint}"
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
        // Lukitse hash-vakaus: jos tämä muuttuu, kaikki tallennetut vektorit
        // muuttuvat → tarkoituksellinen rikkova muutos, ei vahinko.
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn trait_object_usable() {
        // Varmista että trait on objektiturvallinen (dyn) — runtime valitsee
        // tarjoajan ajonaikaisesti.
        let provider: Box<dyn EmbeddingProvider> = Box::new(DeterministicEmbedder::new());
        assert_eq!(provider.dimensions(), 256);
        assert_eq!(provider.embed("test").len(), 256);
    }
}
