//! Memory retrieval: relevance computation ([`RetrievalContext`], [`RetrievalResult`]).
//!
//! This is Eternal Thread's **v1 skeleton** for retrieval: a simple
//! relevance function that combines keyword matching, emotional tone
//! matching, and the memory's current retention (Ebbinghaus). Vector
//! search (cosine similarity, HNSW) will come later behind a feature flag
//! — see the [`crate`]-level documentation.
//!
//! ## Relevance composition
//! ```text
//! relevance = (keyword · 0.55 + emotion · 0.25 + importance · 0.20) · retention
//! ```
//! - `keyword` — the word-match ratio between the query and the content/tags,
//! - `emotion` — the ratio of shared emotion dimensions (Eternal Thread
//!   "emotional boost"),
//! - `importance` — the memory's precomputed importance,
//! - `retention` — Ebbinghaus retention at retrieval time (a forgotten
//!   memory gets low weight even if it matches the words).
//!
//! Tombstoned memories are never returned; archived ones are returned
//! weakened.

use serde::{Deserialize, Serialize};

use familyclaw_core::{time, Timestamp};
use familyclaw_emotion::Dimension;

use crate::memory::Memory;

/// Weight of keyword matching in relevance.
const W_KEYWORD: f32 = 0.55;
/// Weight of emotion matching in relevance.
const W_EMOTION: f32 = 0.25;
/// Weight of importance in relevance.
const W_IMPORTANCE: f32 = 0.20;

/// Relevance factor for an archived memory (decay in retrieval).
const ARCHIVED_PENALTY: f32 = 0.5;

/// Threshold for the cosine-similarity vector search weight.
/// If `semantic_weight` > this and both have an embedding, cosine is used.
const VECTOR_SIMILARITY_THRESHOLD: f32 = 0.01;

/// A retrieval query and its constraints.
///
/// Build with [`RetrievalContext::new`] and adjust in builder style. The
/// context is pure data — the actual retrieval is done by
/// [`crate::MemoryStore::retrieve`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalContext {
    /// Text query (keywords). Empty = no text constraint.
    pub query: String,

    /// Emotion dimensions to weight (Eternal Thread emotion boost).
    #[serde(default)]
    pub emotions: Vec<Dimension>,

    /// Tags that must match (all given ones are required). Empty = no tag
    /// constraint.
    #[serde(default)]
    pub required_tags: Vec<String>,

    /// Maximum number of results to return.
    pub limit: usize,

    /// Minimum acceptable relevance (`0.0..=1.0`). Results below this are
    /// filtered out.
    #[serde(default)]
    pub min_relevance: f32,

    /// Whether to include archived memories (weakened). Default `true`.
    #[serde(default = "default_true")]
    pub include_archived: bool,

    /// Weight of semantic search (`0.0..=1.0`).
    /// 0 = pure keyword matching (default, backward compatible),
    /// 1 = pure semantic similarity (bigram Dice).
    /// > 0 with an embedding set → cosine-similarity vector search.
    #[serde(default)]
    pub semantic_weight: f32,

    /// Query embedding vector for vector search.
    /// If set and `semantic_weight > VECTOR_SIMILARITY_THRESHOLD`, retrieval
    /// computes cosine similarity against memories' embeddings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_embedding: Option<Vec<f32>>,
}

/// serde default for `true` fields.
const fn default_true() -> bool {
    true
}

impl RetrievalContext {
    /// Default limit for returned results.
    pub const DEFAULT_LIMIT: usize = 10;

    /// Creates a retrieval context with a text query; other fields get
    /// defaults (`limit = 10`, archived included, no other constraints).
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
            query_embedding: None,
        }
    }

    /// Sets the emotion dimensions to weight.
    #[must_use]
    pub fn with_emotions(mut self, emotions: impl IntoIterator<Item = Dimension>) -> Self {
        self.emotions = emotions.into_iter().collect();
        self
    }

    /// Sets the required tags.
    #[must_use]
    pub fn with_required_tags(mut self, tags: impl IntoIterator<Item = String>) -> Self {
        self.required_tags = tags.into_iter().collect();
        self
    }

    /// Sets the result limit (at least 1).
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }

    /// Sets the relevance threshold (`0.0..=1.0`, clamped).
    #[must_use]
    pub fn with_min_relevance(mut self, min: f32) -> Self {
        self.min_relevance = if min.is_finite() {
            min.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self
    }

    /// Sets whether to include archived memories.
    #[must_use]
    pub fn including_archived(mut self, include: bool) -> Self {
        self.include_archived = include;
        self
    }

    /// Sets the weight of semantic search (`0.0..=1.0`, clamped).
    /// 0 = pure keyword (default), 1 = pure bigram semantics.
    #[must_use]
    pub fn with_semantic_weight(mut self, weight: f32) -> Self {
        self.semantic_weight = weight.clamp(0.0, 1.0);
        self
    }
    /// Sets the query embedding vector for vector search.
    /// If set and `semantic_weight > VECTOR_SIMILARITY_THRESHOLD`, retrieval
    /// computes cosine similarity against memories' embeddings.
    #[must_use]
    pub fn with_query_embedding(mut self, embedding: impl Into<Vec<f32>>) -> Self {
        self.query_embedding = Some(embedding.into());
        self
    }

    /// Whether the memory's tags satisfy the context's requirements.
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

/// A single retrieval result: a memory and its computed relevance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalResult {
    /// The matched memory.
    pub memory: Memory,
    /// Final relevance score (`0.0..=1.0`).
    pub relevance: f32,
}

/// Computes the relevance of a single memory against the retrieval context
/// at time `at`.
///
/// Returns `None` if the memory does not qualify for retrieval (tombstoned,
/// archived when not wanted, or required tags don't match). Otherwise
/// returns relevance `0.0..=1.0`.
///
/// Relevance = (keyword·0.55 + emotion·0.25 + importance·0.20) · retention,
/// with an additional `× ARCHIVED_PENALTY` for archived memories.
/// If `semantic_weight > VECTOR_SIMILARITY_THRESHOLD` and both have an
/// `embedding`, the `semantic` component is replaced by cosine similarity.
#[must_use]
pub fn score(memory: &Memory, ctx: &RetrievalContext, at: Timestamp) -> Option<f32> {
    use crate::memory::MemoryStatus;

    // Tombstoned memories are never returned.
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

    // Vector search: if semantic_weight > threshold AND both have an
    // embedding, replace the semantic component with cosine similarity
    // (scaled to 0.0..=1.0).
    let semantic_for_text = if ctx.semantic_weight > VECTOR_SIMILARITY_THRESHOLD {
        match (&ctx.query_embedding, &memory.embedding) {
            (Some(query_emb), Some(mem_emb)) if ctx.semantic_weight > 0.0 => {
                // Cosine similarity: -1.0..=1.0 → 0.0..=1.0
                let cos = cosine_similarity(query_emb, mem_emb);
                f32::midpoint(cos, 1.0)
            }
            _ => semantic,
        }
    } else {
        semantic
    };

    // Combined text match: keyword × (1-w) + semantic × w
    let text_score = keyword.mul_add(
        1.0 - ctx.semantic_weight,
        semantic_for_text * ctx.semantic_weight,
    );
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

/// The minimum provenance trust factor a low-trust external source can get
/// in retrieval weighting. Prevents a poisoned (but leaked past the gate,
/// or added without going through the gate) external memory from
/// disappearing entirely — it just sinks to the bottom, so an audit can
/// still find it.
const PROVENANCE_TRUST_FLOOR: f32 = 0.1;

/// Like [`score`], but weights the result by the memory's provenance
/// ([`Provenance`](crate::Provenance)) trust.
///
/// Direct experience and derived memories (`trust = 1.0`) keep their score
/// unchanged. A low-trust external source lowers its ranking by a factor of
/// `0.1 + 0.9 · trust` (see `PROVENANCE_TRUST_FLOOR`): an untrusted external
/// claim sinks to the bottom of retrieval results (a retroactive Sleeper
/// Memory Poisoning protection, also for memories that did not pass through
/// the [`ProvenanceGate`](crate::ProvenanceGate)).
///
/// Returns `None` in the same cases as [`score`] (tombstoned, excluded
/// archived, tag filtering).
#[must_use]
pub fn score_with_provenance(
    memory: &Memory,
    ctx: &RetrievalContext,
    at: Timestamp,
) -> Option<f32> {
    let base = score(memory, ctx, at)?;
    let trust = memory.provenance.trust().clamp(0.0, 1.0);
    let factor = PROVENANCE_TRUST_FLOOR + (1.0 - PROVENANCE_TRUST_FLOOR) * trust;
    Some((base * factor).clamp(0.0, 1.0))
}

/// Confidence-weighted retention for retrieval.
///
/// Confirmed memories (confidence=1.0) retain full retention.
/// Claim memories (confidence=0.0) get only a fraction — they have not
/// been verified, so they should not rise in retrieval results.
///
/// Formula: `adjusted = retention · (0.2 + 0.8 · confidence)`
/// - Claim (0.0) → 20% of retention
/// - Evidence (0.7) → 76% of retention
/// - Confirmed (1.0) → 100% of retention
fn adjusted_retention(memory: &Memory, at: Timestamp) -> f32 {
    let base = memory.retention(at).clamp(0.0, 1.0);
    let confidence = memory.confidence.clamp(0.0, 1.0);
    base * (0.2 + 0.8 * confidence)
}

/// Runs retrieval over the given memories: scores, filters by threshold,
/// and returns the top [`RetrievalContext::limit`] results in descending
/// relevance order.
///
/// Ties are broken in favor of freshness (more recent
/// [`last_reinforced_at`](Memory::last_reinforced_at) first), which makes
/// the ordering deterministic.
///
/// `at` is the retrieval time (for retention computation). Use the
/// [`retrieve_now`] wrapper for the current time.
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

/// Like [`retrieve`], but uses the current time for retention computation.
#[must_use]
pub fn retrieve_now<'a, I>(memories: I, ctx: &RetrievalContext) -> Vec<RetrievalResult>
where
    I: IntoIterator<Item = &'a Memory>,
{
    retrieve(memories, ctx, time::now())
}

/// Semantic similarity: partial matching with unigrams.
///
/// Computes how many query words appear *partially* within the memory's
/// words (substring match). This captures "ship" ↔ "shipped",
/// "bridge" ↔ "bridges", etc.
///
/// Common English filler words are filtered out (≤ 2 characters
/// or on the stoplist). Normalized by the number of query words.
///
/// Empty query or content → 0.0.
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

/// Extracts meaningful words: lowercase, filters short words and stopwords.
fn meaningful_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| {
            let lower = w.to_lowercase();
            w.chars().count() > 2 && !is_stopword(&lower)
        })
        .map(str::to_lowercase)
        .collect()
}

/// Common English stopwords that carry no semantic meaning.
fn is_stopword(word: &str) -> bool {
    matches!(
        word,
        "the"
            | "and"
            | "for"
            | "are"
            | "but"
            | "not"
            | "you"
            | "all"
            | "can"
            | "had"
            | "her"
            | "was"
            | "one"
            | "our"
            | "out"
            | "has"
            | "have"
            | "did"
            | "get"
            | "got"
            | "its"
            | "let"
            | "may"
            | "nor"
            | "off"
            | "old"
            | "per"
            | "put"
            | "set"
            | "she"
            | "too"
            | "use"
            | "who"
            | "how"
            | "any"
            | "yet"
    )
}

/// Keyword match: the ratio of matched query words to all query words.
///
/// The comparison is case-insensitive and applies to both the memory's
/// content and its tags. An empty query → neutral `0.5` (no text
/// constraint, neither favors nor penalizes).
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

/// Emotion match: the ratio of shared emotion dimensions to the query's emotions.
///
/// If the query has no emotions → neutral `0.0` (no emotion boost).
/// Otherwise the share of the query's emotions that the memory also activates.
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

/// Splits text into lowercase words; drops short
/// filler words (≤ 1 character) and non-alphanumeric separators.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 1)
        .map(str::to_lowercase)
        .collect()
}

/// Cosine similarity between two vectors.
/// Returns 0.0 if either is missing or dimensions do not match.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let result = dot / (norm_a.sqrt() * norm_b.sqrt());
    result.clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    // Tests compare exactly representable f32 constants — exact comparison is fine.
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
            "matching {s_hit} not greater than non-matching {s_miss}"
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
        // keyword 0.5, emotion 0.0, importance 0.5·0.45=0.225 → relevance > 0.
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
            "emotion match {s_emo} does not boost above {s_plain}"
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
            "archived {archived} not decayed below {baseline}"
        );
        // When archived memories are excluded → None.
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
        assert!(stale < fresh, "stale {stale} not below fresh {fresh}");
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
        assert!((fresh - later).abs() < 1e-6, "protected anchor decayed");
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
        // "rust memory model" matches both → top result.
        assert!(results[0].memory.content.contains("memory"));
        // Descending relevance.
        assert!(results[0].relevance >= results[1].relevance);
        // The python memory doesn't make it into the top 2 (low match).
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
        // Single-character "a" and "x" are filtered out.
        assert!(!toks.contains(&"a".to_string()));
        assert!(!toks.contains(&"x".to_string()));
    }

    #[test]
    fn provenance_weighting_demotes_low_trust_external() {
        use crate::provenance::Provenance;
        let now = time::now();
        let ctx = RetrievalContext::new("report");

        let direct = Memory::builder("the report content")
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .build();
        let external = Memory::builder("the report content")
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .provenance(Provenance::external("web", 0.1))
            .build();

        let s_direct = score_with_provenance(&direct, &ctx, now).expect("direct scored");
        let s_external = score_with_provenance(&external, &ctx, now).expect("external scored");
        assert!(
            s_external < s_direct,
            "low-trust external {s_external} did not sink below direct {s_direct}"
        );
        // But not to zero — an audit can still find it (trust floor).
        assert!(s_external > 0.0);
    }

    #[test]
    fn provenance_weighting_preserves_direct_experience() {
        let now = time::now();
        let ctx = RetrievalContext::new("report");
        let m = Memory::builder("the report content")
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .build();
        // DirectExperience (trust 1.0) → same score as plain score.
        let plain = score(&m, &ctx, now).expect("plain");
        let weighted = score_with_provenance(&m, &ctx, now).expect("weighted");
        assert!((plain - weighted).abs() < 1e-6);
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
