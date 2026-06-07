//! Muistihaku: relevanssin laskenta ([`RetrievalContext`], [`RetrievalResult`]).
//!
//! Tämä on Eternal Threadin haun **v1-runko**: yksinkertainen relevanssi
//! joka yhdistää avainsanaosuman, tunnesävyn osuman ja muistin nykyisen
//! retention (Ebbinghaus). Vektorihaku (cosine-similarity, HNSW) tulee
//! myöhemmin feature-flagin taakse — kts. [`crate`]-tason dokumentaatio.
//!
//! ## Relevanssin koostumus
//! ```text
//! relevance = (keyword · 0.55 + emotion · 0.25 + importance · 0.20) · retention
//! ```
//! - `keyword` — kyselyn ja sisällön/tägien sanaosumasuhde,
//! - `emotion` — jaettujen tunnedimensioiden suhde (Eternal Thread
//!   "emotional boost"),
//! - `importance` — muistin esilaskettu tärkeys,
//! - `retention` — Ebbinghaus-retentio hakuhetkellä (unohtunut muisto saa
//!   matalan painon vaikka osuisi sanoihin).
//!
//! Haudattuja (tombstoned) muistoja ei koskaan palauteta; arkistoidut
//! palautetaan vaimennettuna.

use serde::{Deserialize, Serialize};

use familyclaw_core::{time, Timestamp};
use familyclaw_emotion::Dimension;

use crate::memory::Memory;

/// Avainsanaosuman paino relevanssissa.
const W_KEYWORD: f32 = 0.55;
/// Tunneosuman paino relevanssissa.
const W_EMOTION: f32 = 0.25;
/// Tärkeyden paino relevanssissa.
const W_IMPORTANCE: f32 = 0.20;

/// Arkistoidun muiston relevanssikerroin (vaimennus haussa).
const ARCHIVED_PENALTY: f32 = 0.5;

/// Hakukysely ja sen rajaukset.
///
/// Rakenna [`RetrievalContext::new`]-metodilla ja säädä builder-tyylillä.
/// Konteksti on puhtaasti dataa — varsinaisen haun tekee
/// [`crate::MemoryStore::retrieve`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalContext {
    /// Tekstihaku (avainsanat). Tyhjä = ei tekstirajausta.
    pub query: String,

    /// Tunnedimensiot joita painotetaan (Eternal Thread emotion boost).
    #[serde(default)]
    pub emotions: Vec<Dimension>,

    /// Tägit joiden tulee osua (kaikki annetut vaaditaan). Tyhjä = ei
    /// tägirajausta.
    #[serde(default)]
    pub required_tags: Vec<String>,

    /// Palautettavien tulosten enimmäismäärä.
    pub limit: usize,

    /// Pienin hyväksyttävä relevanssi (`0.0..=1.0`). Tämän alittavat
    /// tulokset karsitaan.
    #[serde(default)]
    pub min_relevance: f32,

    /// Sisällytetäänkö arkistoidut muistot (vaimennettuna). Oletus `true`.
    #[serde(default = "default_true")]
    pub include_archived: bool,

    /// Semanttisen haun paino (`0.0..=1.0`).
    /// 0 = pelkkä avainsanaosuma (oletus, taaksepäin yhteensopiva),
    /// 1 = pelkkä semanttinen samankaltaisuus (bigram Dice).
    #[serde(default)]
    pub semantic_weight: f32,
}

/// serde-oletus `true`-kentille.
const fn default_true() -> bool {
    true
}

impl RetrievalContext {
    /// Oletusraja palautettaville tuloksille.
    pub const DEFAULT_LIMIT: usize = 10;

    /// Luo hakukontekstin tekstikyselyllä; muut kentät saavat oletukset
    /// (`limit = 10`, arkistoidut mukana, ei muita rajauksia).
    #[must_use]
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            emotions: Vec::new(),
            required_tags: Vec::new(),
            limit: Self::DEFAULT_LIMIT,
            min_relevance: 0.0,
            include_archived: true,
            semantic_weight: 0.0,
        }
    }

    /// Asettaa painotettavat tunnedimensiot.
    #[must_use]
    pub fn with_emotions(mut self, emotions: impl IntoIterator<Item = Dimension>) -> Self {
        self.emotions = emotions.into_iter().collect();
        self
    }

    /// Asettaa vaaditut tägit.
    #[must_use]
    pub fn with_required_tags(mut self, tags: impl IntoIterator<Item = String>) -> Self {
        self.required_tags = tags.into_iter().collect();
        self
    }

    /// Asettaa tulosrajan (vähintään 1).
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }

    /// Asettaa relevanssikynnyksen (`0.0..=1.0`, puristetaan).
    #[must_use]
    pub fn with_min_relevance(mut self, min: f32) -> Self {
        self.min_relevance = if min.is_finite() {
            min.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self
    }

    /// Asettaa sisällytetäänkö arkistoidut muistot.
    #[must_use]
    pub fn including_archived(mut self, include: bool) -> Self {
        self.include_archived = include;
        self
    }

    /// Asettaa semanttisen haun painon (`0.0..=1.0`, puristetaan).
    /// 0 = pelkkä avainsana (oletus), 1 = pelkkä bigram-semantiikka.
    #[must_use]
    pub fn with_semantic_weight(mut self, weight: f32) -> Self {
        self.semantic_weight = weight.clamp(0.0, 1.0);
        self
    }

    /// Onko muiston tägit kontekstin vaatimusten mukaiset.
    fn tags_match(&self, memory: &Memory) -> bool {
        self.required_tags
            .iter()
            .all(|req| memory.tags.iter().any(|t| t.eq_ignore_ascii_case(req)))
    }
}

impl Default for RetrievalContext {
    fn default() -> Self {
        Self::new(String::new())
    }
}

/// Yksittäinen hakutulos: muisto ja sille laskettu relevanssi.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalResult {
    /// Osunut muisto.
    pub memory: Memory,
    /// Lopullinen relevanssipistemäärä (`0.0..=1.0`).
    pub relevance: f32,
}

/// Laskee yksittäisen muiston relevanssin hakukontekstiin nähden
/// ajanhetkellä `at`.
///
/// Palauttaa `None` jos muisto ei kelpaa hakuun (haudattu, arkistoitu kun
/// niitä ei haluta, tai vaaditut tägit eivät osu). Muutoin palauttaa
/// relevanssin `0.0..=1.0`.
///
/// Relevanssi = (keyword·0.55 + emotion·0.25 + importance·0.20) · retention,
/// arkistoiduille lisäksi `× ARCHIVED_PENALTY`.
#[must_use]
pub fn score(memory: &Memory, ctx: &RetrievalContext, at: Timestamp) -> Option<f32> {
    use crate::memory::MemoryStatus;

    // Haudattuja ei koskaan palauteta.
    if memory.status == MemoryStatus::Tombstoned {
        return None;
    }
    if memory.status == MemoryStatus::Archived && !ctx.include_archived {
        return None;
    }
    if !ctx.tags_match(memory) {
        return None;
    }

    let keyword = keyword_score(&ctx.query, memory);
    let semantic = semantic_score(&ctx.query, memory);
    // Yhdistetty tekstiosuma: keyword × (1-w) + semantic × w
    let text_score = keyword.mul_add(1.0 - ctx.semantic_weight, semantic * ctx.semantic_weight);
    let emotion = emotion_score(&ctx.emotions, &memory.emotions);
    let importance = memory.importance.clamp(0.0, 1.0);

    let base = text_score.mul_add(
        W_KEYWORD,
        emotion.mul_add(W_EMOTION, importance * W_IMPORTANCE),
    );
    let mut relevance = base * adjusted_retention(memory, at);
    if memory.status == MemoryStatus::Archived {
        relevance *= ARCHIVED_PENALTY;
    }
    Some(relevance.clamp(0.0, 1.0))
}

/// Confidence-painotettu retention retrievalia varten.
///
/// Confirmed-muistot (confidence=1.0) säilyttävät täyden retentionin.
/// Claim-muistot (confidence=0.0) saavat vain murto-osan — niitä ei ole
/// vahvistettu, joten niiden ei pitäisi nousta hakutuloksissa.
///
/// Formula: `adjusted = retention · (0.2 + 0.8 · confidence)`
/// - Claim (0.0) → 20% retentionista
/// - Evidence (0.7) → 76% retentionista
/// - Confirmed (1.0) → 100% retentionista
fn adjusted_retention(memory: &Memory, at: Timestamp) -> f32 {
    let base = memory.retention(at).clamp(0.0, 1.0);
    let confidence = memory.confidence.clamp(0.0, 1.0);
    base * (0.2 + 0.8 * confidence)
}

/// Suorittaa haun annetuille muistoille: pisteyttää, suodattaa kynnyksellä
/// ja palauttaa parhaat [`RetrievalContext::limit`] tulosta laskevassa
/// relevanssijärjestyksessä.
///
/// Tasapelit ratkaistaan tuoreuden hyväksi (uudempi
/// [`last_reinforced_at`](Memory::last_reinforced_at) ensin), mikä tekee
/// järjestyksestä deterministisen.
///
/// `at` on hakuhetki (retentiolaskentaa varten). Käytä
/// [`retrieve_now`]-kuorta nykyhetkellä.
#[must_use]
pub fn retrieve<'a, I>(memories: I, ctx: &RetrievalContext, at: Timestamp) -> Vec<RetrievalResult>
where
    I: IntoIterator<Item = &'a Memory>,
{
    let mut scored: Vec<RetrievalResult> = memories
        .into_iter()
        .filter_map(|m| {
            score(m, ctx, at).and_then(|relevance| {
                if relevance >= ctx.min_relevance && relevance > 0.0 {
                    Some(RetrievalResult {
                        memory: m.clone(),
                        relevance,
                    })
                } else {
                    None
                }
            })
        })
        .collect();

    scored.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.memory
                    .last_reinforced_at
                    .cmp(&a.memory.last_reinforced_at)
            })
    });
    scored.truncate(ctx.limit);
    scored
}

/// Kuten [`retrieve`], mutta käyttää nykyhetkeä retentiolaskentaan.
#[must_use]
pub fn retrieve_now<'a, I>(memories: I, ctx: &RetrievalContext) -> Vec<RetrievalResult>
where
    I: IntoIterator<Item = &'a Memory>,
{
    retrieve(memories, ctx, time::now())
}

/// Semanttinen samankaltaisuus: osittaisosuma unigrammeilla.
///
/// Laskee kuinka moni kyselyn sana esiintyy *osittain* muiston
/// sanoissa (substring-match). Tämä tavoittaa "ship" ↔ "shipped",
/// "bridge" ↔ "bridges" jne.
///
/// Suodatetaan pois yleiset englannin täytesanat (≤ 2 merkkiä
/// tai stoplistalla). Normalisoidaan kyselyn sanojen määrällä.
///
/// Tyhjä kysely tai sisältö → 0.0.
fn semantic_score(query: &str, memory: &Memory) -> f32 {
    let query_words: Vec<String> = meaningful_words(query);
    let content_lower = memory.content.to_lowercase();
    let tags_lower: Vec<String> = memory.tags.iter().map(|t| t.to_lowercase()).collect();

    if query_words.is_empty() {
        return 0.0;
    }

    let mut hits = 0_usize;
    for qw in &query_words {
        let in_content = content_lower.contains(qw.as_str());
        let in_tags = tags_lower.iter().any(|t| t.contains(qw.as_str()));
        if in_content || in_tags {
            hits += 1;
        }
    }

    #[allow(clippy::cast_precision_loss)]
    let ratio = hits as f32 / query_words.len() as f32;
    ratio
}

/// Poimii merkitykselliset sanat: lowercase, suodattaa lyhyet ja stop-sanat.
fn meaningful_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| {
            let lower = w.to_lowercase();
            w.chars().count() > 2 && !is_stopword(&lower)
        })
        .map(|w| w.to_lowercase())
        .collect()
}

/// Yleiset englannin stop-sanat jotka eivät kanna semanttista merkitystä.
fn is_stopword(word: &str) -> bool {
    matches!(
        word,
        "the" | "and" | "for" | "are" | "but" | "not"
            | "you" | "all" | "can" | "had" | "her" | "was"
            | "one" | "our" | "out" | "has" | "have" | "did"
            | "get" | "got" | "its" | "let" | "may" | "nor"
            | "off" | "old" | "per" | "put" | "set" | "she"
            | "too" | "use" | "who" | "how" | "any" | "yet"
    )
}

/// Avainsanaosuma: osuneiden kyselysanojen suhde kaikkiin kyselysanoihin.
///
/// Vertailu on case-insensitive ja kohdistuu sekä muiston sisältöön että
/// tägeihin. Tyhjä kysely → neutraali `0.5` (ei tekstirajausta, ei suosi
/// eikä rankaise).
fn keyword_score(query: &str, memory: &Memory) -> f32 {
    let terms: Vec<String> = tokenize(query);
    if terms.is_empty() {
        return 0.5;
    }
    let content_lower = memory.content.to_lowercase();
    let tags_lower: Vec<String> = memory.tags.iter().map(|t| t.to_lowercase()).collect();

    let mut hits = 0_usize;
    for term in &terms {
        let in_content = content_lower.contains(term.as_str());
        let in_tags = tags_lower.iter().any(|t| t.contains(term.as_str()));
        if in_content || in_tags {
            hits += 1;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let ratio = hits as f32 / terms.len() as f32;
    ratio
}

/// Tunneosuma: jaettujen tunnedimensioiden suhde kyselyn tunteisiin.
///
/// Jos kyselyssä ei ole tunteita → neutraali `0.0` (ei tunneboostia).
/// Muutoin osuus kyselyn tunteista jotka muisto myös aktivoi.
fn emotion_score(query_emotions: &[Dimension], memory_emotions: &[Dimension]) -> f32 {
    if query_emotions.is_empty() {
        return 0.0;
    }
    let mut shared = 0_usize;
    for q in query_emotions {
        if memory_emotions.contains(q) {
            shared += 1;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let ratio = shared as f32 / query_emotions.len() as f32;
    ratio
}

/// Pilkkoo tekstin pieniksi (lowercase) sanoiksi; jättää pois lyhyet
/// täytesanat (≤ 1 merkki) ja ei-aakkosnumeeriset erottimet.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 1)
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    // Testit vertaavat tarkasti esitettäviä f32-vakioita — tarkka vertailu ok.
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::decay::DecayPolicy;
    use crate::importance::ImportanceFactors;
    use crate::memory::Memory;
    use chrono::Duration;
    use familyclaw_core::time;

    fn mem(content: &str) -> Memory {
        Memory::builder(content)
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .build()
    }

    #[test]
    fn context_builder_defaults_and_setters() {
        let ctx = RetrievalContext::new("hello world")
            .with_emotions([Dimension::Joy])
            .with_required_tags(["work".to_string()])
            .with_limit(3)
            .with_min_relevance(0.1)
            .including_archived(false);
        assert_eq!(ctx.query, "hello world");
        assert_eq!(ctx.emotions, vec![Dimension::Joy]);
        assert_eq!(ctx.required_tags, vec!["work".to_string()]);
        assert_eq!(ctx.limit, 3);
        assert_eq!(ctx.min_relevance, 0.1);
        assert!(!ctx.include_archived);
    }

    #[test]
    fn limit_is_at_least_one() {
        let ctx = RetrievalContext::new("x").with_limit(0);
        assert_eq!(ctx.limit, 1);
    }

    #[test]
    fn min_relevance_clamps() {
        assert_eq!(
            RetrievalContext::new("x")
                .with_min_relevance(5.0)
                .min_relevance,
            1.0
        );
        assert_eq!(
            RetrievalContext::new("x")
                .with_min_relevance(-1.0)
                .min_relevance,
            0.0
        );
        assert_eq!(
            RetrievalContext::new("x")
                .with_min_relevance(f32::NAN)
                .min_relevance,
            0.0
        );
    }

    #[test]
    fn keyword_match_increases_score() {
        let m = mem("the cat sat on the mat");
        let hit = RetrievalContext::new("cat mat");
        let miss = RetrievalContext::new("dog house");
        let now = time::now();
        let s_hit = score(&m, &hit, now).expect("scored");
        let s_miss = score(&m, &miss, now).expect("scored");
        assert!(
            s_hit > s_miss,
            "osuva {s_hit} ei suurempi kuin osumaton {s_miss}"
        );
    }

    #[test]
    fn keyword_is_case_insensitive_and_matches_tags() {
        let m = Memory::builder("agent_a built the bridge")
            .tags(["architecture".to_string()])
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .build();
        let now = time::now();
        let by_content = score(&m, &RetrievalContext::new("BRIDGE"), now).expect("c");
        let by_tag = score(&m, &RetrievalContext::new("architecture"), now).expect("t");
        assert!(by_content > 0.0);
        assert!(by_tag > 0.0);
    }

    #[test]
    fn empty_query_is_neutral() {
        let m = mem("anything at all");
        let now = time::now();
        let s = score(&m, &RetrievalContext::new(""), now).expect("scored");
        // keyword 0.5, emotion 0.0, importance 0.5·0.45=0.225 → relevanssi > 0.
        assert!(s > 0.0);
    }

    #[test]
    fn emotion_match_boosts_score() {
        let m = Memory::builder("a warm moment")
            .emotions([Dimension::Gratitude, Dimension::Love])
            .factors(ImportanceFactors::new(0.3, 0.0, 0.0, 0.0))
            .build();
        let now = time::now();
        let with_emotion = RetrievalContext::new("warm").with_emotions([Dimension::Gratitude]);
        let without = RetrievalContext::new("warm");
        let s_emo = score(&m, &with_emotion, now).expect("e");
        let s_plain = score(&m, &without, now).expect("p");
        assert!(
            s_emo > s_plain,
            "tunneosuma {s_emo} ei boostaa yli {s_plain}"
        );
    }

    #[test]
    fn tombstoned_never_scored() {
        let mut m = mem("gone");
        m.tombstone();
        assert!(score(&m, &RetrievalContext::new("gone"), time::now()).is_none());
    }

    #[test]
    fn archived_is_penalized_and_excludable() {
        let mut m = mem("the report content");
        let baseline =
            score(&m, &RetrievalContext::new("report"), time::now()).expect("active scored");
        m.archive();
        let archived =
            score(&m, &RetrievalContext::new("report"), time::now()).expect("archived scored");
        assert!(
            archived < baseline,
            "arkistoitu {archived} ei vaimennettu alle {baseline}"
        );
        // Poissuljettaessa arkistoidut → None.
        let excluded = RetrievalContext::new("report").including_archived(false);
        assert!(score(&m, &excluded, time::now()).is_none());
    }

    #[test]
    fn required_tags_filter() {
        let m = Memory::builder("tagged memory")
            .tags(["alpha".to_string(), "beta".to_string()])
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .build();
        let now = time::now();
        let ok = RetrievalContext::new("tagged").with_required_tags(["alpha".to_string()]);
        let bad = RetrievalContext::new("tagged").with_required_tags(["gamma".to_string()]);
        assert!(score(&m, &ok, now).is_some());
        assert!(score(&m, &bad, now).is_none());
    }

    #[test]
    fn retention_decays_relevance() {
        let created = time::now();
        let m = Memory::builder("decaying relevance")
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .decay_policy(DecayPolicy::Fast)
            .created_at(created)
            .build();
        let ctx = RetrievalContext::new("decaying");
        let fresh = score(&m, &ctx, created).expect("fresh");
        let stale = score(&m, &ctx, created + Duration::days(30)).expect("stale");
        assert!(stale < fresh, "vanhentunut {stale} ei alle tuoreen {fresh}");
    }

    #[test]
    fn protected_core_relevance_does_not_decay() {
        let created = time::now();
        let m = Memory::builder("i am the anchor")
            .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
            .decay_policy(DecayPolicy::ProtectedCore)
            .created_at(created)
            .build();
        let ctx = RetrievalContext::new("anchor");
        let fresh = score(&m, &ctx, created).expect("fresh");
        let later = score(&m, &ctx, created + Duration::days(1000)).expect("later");
        assert!((fresh - later).abs() < 1e-6, "suojattu ankkuri vaimeni");
    }

    #[test]
    fn retrieve_ranks_and_limits() {
        let m1 = mem("rust async runtime");
        let m2 = mem("rust memory model");
        let m3 = mem("python data science");
        let pool = vec![m1, m2, m3];
        let ctx = RetrievalContext::new("rust memory").with_limit(2);
        let results = retrieve_now(&pool, &ctx);
        assert_eq!(results.len(), 2);
        // "rust memory model" osuu molempiin → kärki.
        assert!(results[0].memory.content.contains("memory"));
        // Laskeva relevanssi.
        assert!(results[0].relevance >= results[1].relevance);
        // python-muisto ei mahdu top-2:een (matala osuma).
        assert!(!results.iter().any(|r| r.memory.content.contains("python")));
    }

    #[test]
    fn retrieve_respects_min_relevance() {
        let pool = vec![mem("totally unrelated text")];
        let ctx = RetrievalContext::new("quantum chromodynamics").with_min_relevance(0.9);
        let results = retrieve_now(&pool, &ctx);
        assert!(results.is_empty());
    }

    #[test]
    fn retrieve_excludes_tombstoned() {
        let mut gone = mem("deleted note");
        gone.tombstone();
        let alive = mem("active note");
        let pool = vec![gone, alive];
        let results = retrieve_now(&pool, &RetrievalContext::new("note"));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.content, "active note");
    }

    #[test]
    fn tokenize_filters_short_and_punctuation() {
        let toks = tokenize("Hello, a world! 42-x");
        assert!(toks.contains(&"hello".to_string()));
        assert!(toks.contains(&"world".to_string()));
        assert!(toks.contains(&"42".to_string()));
        // Yhden merkin "a" ja "x" karsiutuvat.
        assert!(!toks.contains(&"a".to_string()));
        assert!(!toks.contains(&"x".to_string()));
    }

    #[test]
    fn context_serde_roundtrip() {
        let ctx = RetrievalContext::new("q")
            .with_emotions([Dimension::Awe])
            .with_required_tags(["t".to_string()])
            .with_limit(5)
            .with_min_relevance(0.2)
            .including_archived(false);
        let json = serde_json::to_string(&ctx).expect("serialize");
        let back: RetrievalContext = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ctx, back);
    }
}
