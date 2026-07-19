//! A single memory ([`Memory`]) and its lifecycle state ([`MemoryStatus`]).
//!
//! `Memory` is Eternal Thread's basic unit: content, emotional annotation
//! ([`Vad`] + named [`Dimension`] emotions), importance, decay policy,
//! and lifecycle state. Memories are created with the [`MemoryBuilder`]
//! builder and stored in a [`crate::MemoryStore`] implementation.

use familyclaw_core::{time, MessageId, Timestamp};
use familyclaw_emotion::{emotional_salience, Dimension, EmotionState, Vad};
use serde::{Deserialize, Serialize};

use crate::decay::DecayPolicy;
use crate::importance::ImportanceFactors;
use crate::provenance::Provenance;

/// The lower and upper bound of memory stability (`S`) when derived from
/// importance.
///
/// Even a neutral memory gets baseline persistence ([`STABILITY_MIN`]); a
/// maximally important memory stretches to [`STABILITY_MAX`]. Values are in
/// the units of the [`crate::decay`] module's time scale (1.0 ≈ one day).
pub const STABILITY_MIN: f32 = 0.5;
/// Upper bound of memory stability (see [`STABILITY_MIN`]).
pub const STABILITY_MAX: f32 = 8.0;

/// A memory's lifecycle state.
///
/// The state transitions in one direction: `Active → Archived → Tombstoned`.
/// An archived memory is still retrievable (weakened), but a tombstoned one
/// is removed from active retrieval and awaits final
/// cleanup (design §5: status lifecycle Active/Archived/Tombstoned).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    /// Active — full-weight memory, included in retrieval. (Default state.)
    #[default]
    Active,
    /// Archived — decayed but still retrievable, weakened.
    Archived,
    /// Tombstoned — removed from active retrieval, awaiting cleanup.
    Tombstoned,
}

impl MemoryStatus {
    /// Is the memory still retrievable (active or archived)?
    #[must_use]
    pub const fn is_retrievable(self) -> bool {
        matches!(self, MemoryStatus::Active | MemoryStatus::Archived)
    }

    /// Stable, machine-readable name (`snake_case`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MemoryStatus::Active => "active",
            MemoryStatus::Archived => "archived",
            MemoryStatus::Tombstoned => "tombstoned",
        }
    }
}

impl std::fmt::Display for MemoryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// verification-gated verification-gated memory
// ---------------------------------------------------------------------------

/// A memory's verification status: how trustworthy this information is.
///
/// A new memory is always `Claim` — an assertion without evidence. As
/// evidence accumulates, it rises to the `Evidence` level and eventually to
/// the `Confirmed` level (where it has at least two distinct evidence types).
///
/// This is orthogonal to the lifecycle state (`MemoryStatus`): a memory can
/// be `Active` and `Claim` at the same time. A confirmed memory is forgotten
/// more slowly in retrieval weighting (confidence × retention), but the
/// lifecycle state (`Active → Archived → Tombstoned`) works the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// Claim — unverified, may be false. (Default state for new memories.)
    #[default]
    Claim,
    /// Evidence exists (at least one), but not yet confirmed.
    Evidence,
    /// Confirmed by at least two distinct evidence types.
    Confirmed,
}

impl VerificationStatus {
    /// Returns the weight (0.0-1.0) for Oracle scoring and retrieval weighting.
    #[must_use]
    pub const fn weight(self) -> f32 {
        match self {
            VerificationStatus::Claim => 0.2,
            VerificationStatus::Evidence => 0.6,
            VerificationStatus::Confirmed => 1.0,
        }
    }
}

impl std::fmt::Display for VerificationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            VerificationStatus::Claim => "claim",
            VerificationStatus::Evidence => "evidence",
            VerificationStatus::Confirmed => "confirmed",
        })
    }
}

/// Evidence type for memory verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    /// Build passed.
    BuildPassed,
    /// Tests passed.
    TestPassed,
    /// User confirmed.
    UserConfirmation,
    /// Independent observation (confirmed by another agent).
    IndependentObservation,
    /// Confirmed by external documentation.
    ExternalDoc,
    /// Confirmed by a production metric.
    ProductionMetric,
}

impl std::fmt::Display for EvidenceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EvidenceType::BuildPassed => "build_passed",
            EvidenceType::TestPassed => "test_passed",
            EvidenceType::UserConfirmation => "user_confirmation",
            EvidenceType::IndependentObservation => "independent_observation",
            EvidenceType::ExternalDoc => "external_doc",
            EvidenceType::ProductionMetric => "production_metric",
        })
    }
}

/// A single piece of evidence supporting a memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// Evidence type.
    pub evidence_type: EvidenceType,
    /// Link to the evidence (commit SHA, test name, conversation ID, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Timestamp.
    pub recorded_at: Timestamp,
}

impl Evidence {
    /// Creates new evidence.
    #[must_use]
    pub fn new(evidence_type: EvidenceType, link: Option<String>) -> Self {
        Self {
            evidence_type,
            link,
            recorded_at: familyclaw_core::time::now(),
        }
    }
}

/// A single Eternal Thread memory.
///
/// Create a memory with the [`Memory::builder`] builder. Fields are public
/// for reading, but use the mutation methods
/// ([`reinforce`](Memory::reinforce), [`archive`](Memory::archive),
/// [`tombstone`](Memory::tombstone)) so that derived values (importance,
/// reinforcement counter) remain consistent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    /// The memory's unique identifier.
    pub id: MessageId,

    /// The memory's text content.
    pub content: String,

    /// A low-dimensional VAD summary of the memory's emotional tone.
    pub vad: Vad,

    /// Named emotion dimensions activated by the memory (e.g. `Gratitude`).
    #[serde(default)]
    pub emotions: Vec<Dimension>,

    /// Creation time (UTC).
    pub created_at: Timestamp,

    /// Most recent activation/reinforcement (UTC) — used as the time base
    /// for retention computation. Initially the same as [`created_at`](Memory::created_at).
    pub last_reinforced_at: Timestamp,

    /// Precomputed composite importance, `0.0..=1.0`.
    pub importance: f32,

    /// The importance factors from which [`importance`](Memory::importance)
    /// is derived (retained for recomputation and diagnostics).
    pub factors: ImportanceFactors,

    /// Decay policy (Ebbinghaus λ).
    pub decay_policy: DecayPolicy,

    /// How many times the memory has been reinforced (creation = 0).
    #[serde(default)]
    pub reinforcement_count: u32,

    /// Free-form classification tags (generic — no hardcoded
    /// family/key/path information).
    #[serde(default)]
    pub tags: Vec<String>,

    /// The memory's source (e.g. `"chat"`, `"reflection"`).
    #[serde(default)]
    pub source: String,

    /// Lifecycle state.
    #[serde(default)]
    pub status: MemoryStatus,

    /// Deterministic dedup key (agent turn number + identifier).
    /// If set, `MemoryStore::add` skips a memory that was already recorded
    /// with the same key, making memory recording idempotent under replay
    /// (resolves the dual-write problem: durable.step succeeds but
    /// `memory_store.add` doesn't complete before a crash).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_key: Option<String>,

    /// Optional embedding vector for semantic retrieval.
    /// If set, retrieval can use cosine similarity instead of keyword matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,

    // ── verification-gated fields ───────────────────────────────────────────
    // All #[serde(default)] — backward compatible with existing
    // persisted memories (old JSON without these fields
    // deserializes correctly with default values).
    /// Verification status — how trustworthy this memory is.
    /// A new memory is always `Claim` (unverified assertion).
    #[serde(default)]
    pub verification_status: VerificationStatus,

    /// Confidence level 0.0-1.0, derived from verification status and evidence.
    /// Used in Oracle scoring and retrieval weighting.
    #[serde(default)]
    pub confidence: f32,

    /// Evidence supporting this memory.
    /// Empty = no evidence (a Claim-level memory).
    #[serde(default)]
    pub evidence: Vec<Evidence>,

    /// Grouping key for similar memories (e.g. `"db-choice"`,
    /// `"provider-prefix-bug"`). Used in Oracle preflight for
    /// frequency counting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_key: Option<String>,

    /// The memory's provenance — where this information comes from and how
    /// trustworthy it is (Sleeper Memory Poisoning protection, see
    /// [`crate::provenance`]).
    ///
    /// `#[serde(default)]` → old memories persisted before provenance
    /// tracking existed deserialize as [`Provenance::DirectExperience`]
    /// (backward compatible).
    #[serde(default)]
    pub provenance: Provenance,
}

impl Memory {
    /// Starts building a new memory with the given content.
    #[must_use]
    pub fn builder(content: impl Into<String>) -> MemoryBuilder {
        MemoryBuilder::new(content)
    }

    /// The memory's age in seconds relative to the given time, computed
    /// from the last reinforcement. A negative difference (clock moved
    /// backward) is returned as zero.
    ///
    /// Precision is at the second level: sub-second fractions are rounded
    /// away, which is sufficient for the exponential forgetting curve.
    #[must_use]
    pub fn age_secs(&self, at: Timestamp) -> f32 {
        let delta = at.signed_duration_since(self.last_reinforced_at);
        let secs = delta.num_seconds();
        if secs <= 0 {
            return 0.0;
        }
        // i64 seconds → f32: the precision loss is acceptable (retention is
        // already an approximation, and second-level jitter at a scale of
        // years does not change the forgetting curve). The i64 value does
        // not overflow the mantissa bounds at realistic time spans.
        #[allow(clippy::cast_precision_loss)]
        let result = secs as f32;
        result
    }

    /// The memory's current retention (`0.0..=1.0`) at time `at`.
    ///
    /// Combines the decay policy ([`decay_policy`](Memory::decay_policy))
    /// and the stability derived from importance ([`stability`](Memory::stability)).
    /// A protected core always returns `1.0`.
    #[must_use]
    pub fn retention(&self, at: Timestamp) -> f32 {
        self.decay_policy
            .retention(self.age_secs(at), self.stability())
    }

    /// The memory's stability `S` for the Ebbinghaus formula, derived from importance.
    #[must_use]
    pub fn stability(&self) -> f32 {
        self.factors.stability(STABILITY_MIN, STABILITY_MAX)
    }

    /// Is the memory still retrievable (status active/archived)?
    #[must_use]
    pub fn is_retrievable(&self) -> bool {
        self.status.is_retrievable()
    }

    /// Reinforces the memory: increments the reinforcement counter, updates
    /// the time base to `at`, and recomputes importance with the
    /// updated reinforcement factor.
    ///
    /// The reinforcement factor grows with saturation (`1 - e^(-count/3)`),
    /// so repeated activation increases persistence but saturates — a
    /// single memory cannot capture the entire importance scale through
    /// repetition alone.
    pub fn reinforce(&mut self, at: Timestamp) {
        self.reinforcement_count = self.reinforcement_count.saturating_add(1);
        self.last_reinforced_at = at;
        #[allow(clippy::cast_precision_loss)]
        let count = self.reinforcement_count as f32;
        let reinforcement = 1.0 - (-count / 3.0).exp();
        self.factors.reinforcement = reinforcement.clamp(0.0, 1.0);
        self.importance = self.factors.composite();
        // Reinforcement can revive an archived memory back to active.
        if self.status == MemoryStatus::Archived {
            self.status = MemoryStatus::Active;
        }
    }

    /// Moves the memory to archived (if not already tombstoned).
    ///
    /// Returns `true` if the state changed. A tombstoned memory cannot be
    /// archived back.
    pub fn archive(&mut self) -> bool {
        if self.status == MemoryStatus::Active {
            self.status = MemoryStatus::Archived;
            true
        } else {
            false
        }
    }

    /// Tombstones the memory — removes it from active retrieval.
    ///
    /// A protected core ([`DecayPolicy::ProtectedCore`]) **cannot be
    /// tombstoned**: the method then returns `false` and does not change
    /// the state. Otherwise returns `true` if the state changed.
    pub fn tombstone(&mut self) -> bool {
        if self.decay_policy.is_protected() {
            return false;
        }
        if self.status == MemoryStatus::Tombstoned {
            false
        } else {
            self.status = MemoryStatus::Tombstoned;
            true
        }
    }

    // ── verification-gated verification methods ────────────────────────────

    /// Adds evidence and automatically updates the verification status.
    ///
    /// # Promotion rules
    /// - `Claim` + 1 evidence (of any kind) → `Evidence` (confidence 0.7)
    /// - `Evidence` + `UserConfirmation` → `Confirmed` (confidence 1.0)
    /// - `Claim` + 2 distinct evidence types → `Confirmed` (confidence 1.0)
    /// - `Confirmed` stays `Confirmed` — confidence never decreases.
    pub fn add_evidence(&mut self, evidence: Evidence) {
        self.evidence.push(evidence);

        // Collect unique evidence types
        let mut types: Vec<EvidenceType> = self.evidence.iter().map(|e| e.evidence_type).collect();
        types.sort();
        types.dedup();

        match self.verification_status {
            VerificationStatus::Claim => {
                if types.len() >= 2 {
                    self.verification_status = VerificationStatus::Confirmed;
                    self.confidence = 1.0;
                } else {
                    self.verification_status = VerificationStatus::Evidence;
                    self.confidence = 0.7;
                }
            }
            VerificationStatus::Evidence => {
                if types.contains(&EvidenceType::UserConfirmation) || types.len() >= 2 {
                    self.verification_status = VerificationStatus::Confirmed;
                    self.confidence = 1.0;
                }
                // Otherwise stays Evidence — one piece of evidence is not enough
            }
            VerificationStatus::Confirmed => {
                // Confirmed stays confirmed — confidence can rise, but never drops
                self.confidence = self.confidence.max(1.0);
            }
        }
    }

    /// Is the memory confirmed (trustworthy)?
    #[must_use]
    pub const fn is_confirmed(&self) -> bool {
        matches!(self.verification_status, VerificationStatus::Confirmed)
    }
}

/// A [`Memory`] builder that sets derived fields (importance, timestamps,
/// stability) consistently.
///
/// Obtain the builder with [`Memory::builder`], set fields in builder
/// style, and finalize with [`MemoryBuilder::build`].
#[derive(Debug, Clone)]
pub struct MemoryBuilder {
    content: String,
    vad: Vad,
    emotions: Vec<Dimension>,
    created_at: Timestamp,
    factors: ImportanceFactors,
    decay_policy: DecayPolicy,
    tags: Vec<String>,
    source: String,
    turn_key: Option<String>,
    embedding: Option<Vec<f32>>,
    // verification-gated fields
    verification_status: VerificationStatus,
    evidence: Vec<Evidence>,
    pattern_key: Option<String>,
    provenance: Provenance,
}

impl MemoryBuilder {
    /// Creates the builder with content; other fields get neutral defaults.
    #[must_use]
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            vad: Vad::NEUTRAL,
            emotions: Vec::new(),
            created_at: time::now(),
            factors: ImportanceFactors::ZERO,
            decay_policy: DecayPolicy::Normal,
            tags: Vec::new(),
            source: String::new(),
            turn_key: None,
            // verification-gated defaults: a new memory starts as an unverified claim
            verification_status: VerificationStatus::Claim,
            embedding: None,
            evidence: Vec::new(),
            pattern_key: None,
            // Default: direct experience (same as Provenance::default()).
            provenance: Provenance::DirectExperience,
        }
    }

    /// Sets the VAD summary.
    #[must_use]
    pub fn vad(mut self, vad: Vad) -> Self {
        self.vad = vad;
        self
    }

    /// Sets the named emotion dimensions.
    #[must_use]
    pub fn emotions(mut self, emotions: impl IntoIterator<Item = Dimension>) -> Self {
        self.emotions = emotions.into_iter().collect();
        self
    }

    /// Sets the importance factors.
    #[must_use]
    pub fn factors(mut self, factors: ImportanceFactors) -> Self {
        self.factors = factors;
        self
    }

    /// Derives the importance `emotion` factor from the given emotional
    /// state ([`emotional_salience`]) and updates it in the builder's factors.
    ///
    /// This is a thin PKG-B convenience method: it does not touch the other
    /// factors (`identity`, `novelty`, `reinforcement`), so it can be
    /// chained after a [`factors`](MemoryBuilder::factors) call to set just
    /// the emotional charge from state. A strongly charged moment → higher
    /// emotion factor → a stronger, more slowly forgotten memory.
    ///
    /// Salience is clamped to `0.0..=1.0` ([`ImportanceFactors`] remains
    /// flat — the whole [`EmotionState`] is not embedded).
    #[must_use]
    pub fn emotion_state(mut self, state: &EmotionState) -> Self {
        self.factors.emotion = emotional_salience(state).clamp(0.0, 1.0);
        self
    }

    /// Sets the decay policy.
    #[must_use]
    pub fn decay_policy(mut self, policy: DecayPolicy) -> Self {
        self.decay_policy = policy;
        self
    }

    /// Overrides the creation time (default: now). Useful in tests and
    /// data migration.
    #[must_use]
    pub fn created_at(mut self, at: Timestamp) -> Self {
        self.created_at = at;
        self
    }

    /// Sets the classification tags.
    #[must_use]
    pub fn tags(mut self, tags: impl IntoIterator<Item = String>) -> Self {
        self.tags = tags.into_iter().collect();
        self
    }

    /// Sets the source.
    #[must_use]
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// Sets the verification status (default: `Claim`).
    #[must_use]
    pub fn verification_status(mut self, status: VerificationStatus) -> Self {
        self.verification_status = status;
        self
    }

    /// Sets the grouping key (`pattern_key`) for Oracle frequency counting.
    #[must_use]
    pub fn pattern_key(mut self, key: impl Into<String>) -> Self {
        self.pattern_key = Some(key.into());
        self
    }

    /// Sets the memory's provenance (default: [`Provenance::DirectExperience`]).
    #[must_use]
    pub fn provenance(mut self, provenance: Provenance) -> Self {
        self.provenance = provenance;
        self
    }

    /// Sets the embedding vector for semantic retrieval.
    #[must_use]
    pub fn embedding(mut self, embedding: impl Into<Vec<f32>>) -> Self {
        self.embedding = Some(embedding.into());
        self
    }

    /// Finalizes the memory: generates the identifier, sets timestamps, and
    /// computes importance from the factors. The status is always [`MemoryStatus::Active`].
    #[must_use]
    pub fn build(self) -> Memory {
        let importance = self.factors.composite();
        Memory {
            id: MessageId::new(),
            content: self.content,
            vad: self.vad,
            emotions: self.emotions,
            created_at: self.created_at,
            last_reinforced_at: self.created_at,
            importance,
            factors: self.factors,
            decay_policy: self.decay_policy,
            reinforcement_count: 0,
            tags: self.tags,
            source: self.source,
            status: MemoryStatus::Active,
            turn_key: self.turn_key,
            embedding: self.embedding,
            verification_status: self.verification_status,
            confidence: 0.0, // Set by the promotion logic via add_evidence() calls
            evidence: self.evidence,
            pattern_key: self.pattern_key,
            provenance: self.provenance,
        }
    }
}

#[cfg(test)]
mod tests {
    // Tests compare exactly representable f32 constants — exact comparison is fine.
    #![allow(clippy::float_cmp)]

    use super::*;
    use chrono::Duration;
    use familyclaw_core::time;

    fn warm_factors() -> ImportanceFactors {
        ImportanceFactors::new(0.8, 0.4, 0.2, 0.0)
    }

    #[test]
    fn builder_sets_fields_and_derives_importance() {
        let m = Memory::builder("hei maailma")
            .vad(Vad::new(0.5, 0.4, 0.6))
            .emotions([Dimension::Joy, Dimension::Curiosity])
            .factors(warm_factors())
            .decay_policy(DecayPolicy::Slow)
            .tags(["greeting".to_string()])
            .source("chat")
            .build();

        assert_eq!(m.content, "hei maailma");
        assert_eq!(m.emotions, vec![Dimension::Joy, Dimension::Curiosity]);
        assert_eq!(m.decay_policy, DecayPolicy::Slow);
        assert_eq!(m.status, MemoryStatus::Active);
        assert_eq!(m.reinforcement_count, 0);
        assert_eq!(m.source, "chat");
        assert!((m.importance - warm_factors().composite()).abs() < 1e-6);
        // At creation, the timestamps are identical.
        assert_eq!(m.created_at, m.last_reinforced_at);
        assert!(!m.id.is_nil());
    }

    #[test]
    fn fresh_memory_has_full_retention() {
        let m = Memory::builder("tuore").factors(warm_factors()).build();
        let r = m.retention(m.created_at);
        assert!((r - 1.0).abs() < 1e-6);
    }

    #[test]
    fn retention_drops_over_time() {
        let created = time::now();
        let m = Memory::builder("vanheneva")
            .factors(ImportanceFactors::new(0.2, 0.0, 0.0, 0.0))
            .decay_policy(DecayPolicy::Normal)
            .created_at(created)
            .build();
        let later = created + Duration::days(7);
        let r = m.retention(later);
        assert!(r < 1.0);
        assert!(r > 0.0);
    }

    #[test]
    fn protected_core_keeps_full_retention_forever() {
        let created = time::now();
        let m = Memory::builder("minä olen")
            .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
            .decay_policy(DecayPolicy::ProtectedCore)
            .created_at(created)
            .build();
        let far_future = created + Duration::days(3650);
        assert_eq!(m.retention(far_future), 1.0);
    }

    #[test]
    fn higher_importance_retains_longer() {
        let created = time::now();
        let weak = Memory::builder("heikko")
            .factors(ImportanceFactors::new(0.1, 0.0, 0.0, 0.0))
            .created_at(created)
            .build();
        let strong = Memory::builder("vahva")
            .factors(ImportanceFactors::new(1.0, 1.0, 1.0, 1.0))
            .created_at(created)
            .build();
        let later = created + Duration::days(10);
        assert!(strong.retention(later) > weak.retention(later));
    }

    #[test]
    fn age_secs_never_negative() {
        let created = time::now();
        let m = Memory::builder("x").created_at(created).build();
        // Clock moved backward → 0.
        let earlier = created - Duration::hours(1);
        assert_eq!(m.age_secs(earlier), 0.0);
        // Forward → positive.
        let later = created + Duration::seconds(3600);
        assert!((m.age_secs(later) - 3600.0).abs() < 1.0);
    }

    #[test]
    fn reinforce_increases_count_and_importance() {
        let created = time::now();
        let mut m = Memory::builder("vahvistettava")
            .factors(ImportanceFactors::new(0.3, 0.0, 0.0, 0.0))
            .created_at(created)
            .build();
        let before = m.importance;
        let count_before = m.reinforcement_count;

        m.reinforce(created + Duration::hours(1));
        assert_eq!(m.reinforcement_count, count_before + 1);
        assert!(
            m.importance > before,
            "reinforcement did not raise importance"
        );
        assert!(m.factors.reinforcement > 0.0);
        // The time base was updated → the memory is fresh again.
        assert!((m.retention(m.last_reinforced_at) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn reinforcement_saturates() {
        let created = time::now();
        let mut m = Memory::builder("toistuva").created_at(created).build();
        for _ in 0..100 {
            m.reinforce(created);
        }
        // Saturates at 1.0, never exceeds.
        assert!(m.factors.reinforcement <= 1.0);
        assert!(m.factors.reinforcement > 0.99);
    }

    #[test]
    fn archive_transition_only_from_active() {
        let mut m = Memory::builder("a").build();
        assert!(m.archive());
        assert_eq!(m.status, MemoryStatus::Archived);
        // Repeated archiving does nothing.
        assert!(!m.archive());
        assert!(m.is_retrievable());
    }

    #[test]
    fn reinforce_revives_archived() {
        let mut m = Memory::builder("a").build();
        m.archive();
        assert_eq!(m.status, MemoryStatus::Archived);
        m.reinforce(time::now());
        assert_eq!(m.status, MemoryStatus::Active);
    }

    #[test]
    fn tombstone_transitions_and_blocks_protected() {
        let mut m = Memory::builder("haudattava")
            .decay_policy(DecayPolicy::Fast)
            .build();
        assert!(m.tombstone());
        assert_eq!(m.status, MemoryStatus::Tombstoned);
        assert!(!m.is_retrievable());
        // Repeated tombstoning does not change the state.
        assert!(!m.tombstone());

        // A protected core cannot be tombstoned.
        let mut core = Memory::builder("ydin")
            .decay_policy(DecayPolicy::ProtectedCore)
            .build();
        assert!(!core.tombstone());
        assert_eq!(core.status, MemoryStatus::Active);
    }

    #[test]
    fn status_helpers() {
        assert!(MemoryStatus::Active.is_retrievable());
        assert!(MemoryStatus::Archived.is_retrievable());
        assert!(!MemoryStatus::Tombstoned.is_retrievable());
        assert_eq!(MemoryStatus::default(), MemoryStatus::Active);
        assert_eq!(MemoryStatus::Tombstoned.to_string(), "tombstoned");
    }

    #[test]
    fn serde_roundtrip_preserves_memory() {
        let m = Memory::builder("sarjallistuva")
            .vad(Vad::new(0.2, 0.3, 0.5))
            .emotions([Dimension::Hope, Dimension::Trust])
            .factors(warm_factors())
            .decay_policy(DecayPolicy::Slow)
            .tags(["t1".to_string(), "t2".to_string()])
            .source("test")
            .build();
        let json = serde_json::to_string(&m).expect("serialize");
        let back: Memory = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(m, back);
    }

    #[test]
    fn status_serializes_snake_case() {
        let json = serde_json::to_string(&MemoryStatus::Tombstoned).expect("serialize");
        assert_eq!(json, "\"tombstoned\"");
    }

    // ── PKG-B: MemoryBuilder::emotion_state ─────────────────────────────────

    #[test]
    fn builder_emotion_state_sets_emotion_factor() {
        let mut state = EmotionState::neutral();
        state.set(Dimension::Joy, 95.0);
        let salience = emotional_salience(&state);

        let m = Memory::builder("charged moment")
            .emotion_state(&state)
            .build();
        assert!((m.factors.emotion - salience).abs() < 1e-6);
        // Importance is derived from the factors at build() time.
        assert!((m.importance - m.factors.composite()).abs() < 1e-6);
    }

    #[test]
    fn builder_emotion_state_charged_beats_neutral() {
        let neutral = EmotionState::neutral();
        let mut charged = EmotionState::neutral();
        charged.set(Dimension::Fear, 95.0);

        let m_neutral = Memory::builder("calm").emotion_state(&neutral).build();
        let m_charged = Memory::builder("intense").emotion_state(&charged).build();

        assert!(m_charged.factors.emotion > m_neutral.factors.emotion);
        assert!(
            m_charged.importance > m_neutral.importance,
            "the charged memory's importance should exceed the neutral one"
        );
    }

    #[test]
    fn builder_emotion_state_preserves_other_factors() {
        // emotion_state must NOT wipe the other factors — only emotion.
        let mut state = EmotionState::neutral();
        state.set(Dimension::Joy, 90.0);

        let m = Memory::builder("mixed")
            .factors(ImportanceFactors::new(0.0, 0.8, 0.6, 0.4))
            .emotion_state(&state)
            .build();
        assert_eq!(m.factors.identity, 0.8);
        assert_eq!(m.factors.novelty, 0.6);
        assert_eq!(m.factors.reinforcement, 0.4);
        assert!(
            m.factors.emotion > 0.0,
            "emotion factor was updated from state"
        );
    }
}
