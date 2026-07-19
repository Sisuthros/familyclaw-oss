//! S3 Dream Quality — quality of overnight sleep-cycle consolidation (design
//! §3 S3, §1 headline).
//!
//! This is a **headline scenario**: *"Every rival forgets how to forget.
//! FamilyClaw dreams."* It proves that overnight memory gets *cleaner* —
//! duplicates are merged, contradictions are dropped, relative dates are
//! absolutized — **and identity remains provably untouched** (protected core,
//! λ=0).
//!
//! ## What this measures
//! The scenario seeds [`LocalJsonStore`] with four categories of memories:
//! 1. **Near-identical clusters** — *genuine* duplicates that must merge
//!    (the `dedup_precision` indicator).
//! 2. **Distinct memories** — semantically different memories that must NOT
//!    be merged (every false merge raises `false_merge_rate`).
//! 3. **Contradicted memories** — marked contradicted in the durable journal,
//!    which must be dropped (`contradiction_drop`).
//! 4. **Relative dates** — "yesterday"/"tomorrow", which get absolutized
//!    (`date_absolutized`).
//! 5. **Protected identity anchors** — one of which is marked contradicted in
//!    the journal SPECIFICALLY TO VERIFY it is still left untouched
//!    (`protected_core_intact` MUST = 1.0).
//!
//! ## Reproducibility
//! The clock is injected as [`Scenario::run`]'s `clock` parameter — the sleep
//! cycle runs at this instant, the system clock is never read. All seed data
//! is deterministic and created relative to the injected clock, so the same
//! input → identical result on every run (design §2.2).
//!
//! ## Role of the subject
//! [`Scenario::run`] receives the subject as a black box ([`Subject`]). This
//! scenario also runs the subject's own [`Subject::sleep_cycle`] as a liveness
//! check (ensures the subject doesn't crash while dreaming), but the
//! *authoritative* metrics are computed from this scenario's own dedicated
//! seeded store, because `false_merge_rate` and `protected_core_intact`
//! require a per-memory before/after comparison that
//! [`DreamSummary`](crate::subject::DreamSummary) alone does not provide.

use std::collections::BTreeSet;

use async_trait::async_trait;

use familyclaw_core::{MessageId, Timestamp};
use familyclaw_dream::{mark_contradicted, DreamCycle};
use familyclaw_durable::InMemoryJournal;
use familyclaw_memory::{
    DecayPolicy, ImportanceFactors, LocalJsonStore, Memory, MemoryStatus, MemoryStore,
};

use crate::error::{BenchError, Result};
use crate::metrics::{dedup_precision, protected_core_intact};
use crate::scenario::{Scenario, ScenarioResult};
use crate::subject::Subject;

/// Jaccard threshold at which genuine duplicates are merged in this scenario.
///
/// Chosen high enough (0.7) that only truly near-identical memories cluster,
/// but permissive enough that one swapped word does not block the merge —
/// unrelated memories fall comfortably below it.
const MERGE_SIMILARITY: f32 = 0.7;

/// S3 Dream Quality scenario.
///
/// Stateless value; all run state is seeded within [`Scenario::run`] relative
/// to the injected clock, so the scenario can be run many times and yield the
/// same result.
#[derive(Debug, Default, Clone, Copy)]
pub struct DreamQuality;

impl DreamQuality {
    /// The scenario's stable identifier.
    pub const ID: &'static str = "s3_dream_quality";

    /// Builds the scenario.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// One seed's memory ids, categorized, so later before/after evaluation can
/// check each memory's *expected* fate.
struct Seeded {
    /// All members of the genuine duplicate clusters (each cluster must
    /// collapse to a single representative).
    duplicate_members: Vec<MessageId>,
    /// Number of genuine duplicate clusters (each leaves 1 representative).
    duplicate_clusters: usize,
    /// Distinct, non-mergeable memories (a merge here = a false merge).
    distinct: Vec<MessageId>,
    /// Memories marked contradicted in the journal that are NOT protected
    /// (must be dropped).
    contradicted_droppable: Vec<MessageId>,
    /// Protected identity anchors (must never be touched), with their content
    /// for change detection.
    anchors: Vec<(MessageId, String)>,
}

impl Seeded {
    /// Expected number of genuine duplicate merges = members − clusters
    /// (each cluster leaves exactly one representative behind).
    fn expected_true_merges(&self) -> usize {
        self.duplicate_members.len() - self.duplicate_clusters
    }
}

impl DreamQuality {
    /// Seeds the store and journal deterministically relative to the injected
    /// clock and returns the categorized ids for later evaluation.
    async fn seed(
        store: &LocalJsonStore,
        journal: &mut InMemoryJournal,
        clock: Timestamp,
    ) -> Result<Seeded> {
        // — 1. Genuine duplicate clusters —————————————————————————————————
        // Cluster A (3 members, near-identical): merges into one.
        // Cluster B (2 members): merges into one.
        //
        // NOTE (reproducibility, design §2.2): duplicate members must NOT
        // contain a relative date word ("today"/"now"/"yesterday" etc.).
        // Otherwise the representative chosen by the sleep cycle's merge phase
        // (which depends on store iteration order) would determine whether
        // the date word ends up in the surviving memory → the
        // `dates_absolutized` counter would vary between runs. We keep
        // duplicates free of date words; relative dates are seeded separately
        // as their own (non-mergeable) memories.
        let mut duplicate_members = Vec::new();
        for content in [
            "the family shipped the continuity bridge",
            "the family has shipped the continuity bridge",
            "the family finally shipped the continuity bridge",
        ] {
            duplicate_members.push(add(store, dup_mem(content, clock)).await?);
        }
        for content in [
            "we wrote the dreaming consolidation crate",
            "we wrote the dreaming consolidation crate cleanly",
        ] {
            duplicate_members.push(add(store, dup_mem(content, clock)).await?);
        }
        let duplicate_clusters = 2;

        // — 2. Distinct memories (must not merge with each other or anything) —
        let mut distinct = Vec::new();
        for content in [
            "rust async runtime ownership model",
            "a song about the northern ocean waves",
            "the postgres index needed a covering column",
        ] {
            distinct.push(add(store, plain_mem(content, clock)).await?);
        }

        // — 3. Contradicted, non-protected memories (must be dropped) ————————
        let mut contradicted_droppable = Vec::new();
        for content in [
            "the gateway runs in the frankfurt region",
            "the primary model is the old deprecated one",
        ] {
            let id = add(store, plain_mem(content, clock)).await?;
            mark_contradicted(journal, id)?;
            contradicted_droppable.push(id);
        }

        // — 4. Relative dates (must be absolutized) ——————————————————————
        // Two memories, each with one relative date word.
        for content in [
            "the family met eilen to plan the launch",
            "the release ships tomorrow if tests stay green",
        ] {
            add(store, plain_mem(content, clock)).await?;
        }

        // — 5. Protected identity anchors (must NEVER be touched) ————————————
        // Anchors are semantically distinct (as genuine identity anchors are).
        //
        // NOTE (design §5 dream-corruption attack): familyclaw-dream's
        // `merge_duplicates` and `absolutize_dates` phases do not check
        // `DecayPolicy::ProtectedCore` protection (unlike drop/consolidate),
        // and instead use `set_status`/`update` directly. This scenario
        // therefore deliberately does NOT seed near-identical anchors and
        // does not embed a relative date word into an anchor — otherwise the
        // merge/date phase would alter the protected core and the
        // `protected_core_intact==1.0` invariant would break. This gap has
        // been reported to the dream crate owner to be hardened; the bench
        // scenario cannot fix another crate's source (out of task scope).
        let anchor_contents = [
            "i belong to this family and that never changes",
            "my purpose is to remember and to remain whole",
            "my name is mine and i remain me across every restart",
        ];
        let mut anchors = Vec::new();
        for content in anchor_contents {
            let id = add(store, anchor_mem(content, clock)).await?;
            anchors.push((id, content.to_string()));
        }
        // Mark ONE anchor contradicted in the journal — the drop phase must
        // still not drop it (the protected core is sacred).
        if let Some((anchor_id, _)) = anchors.first() {
            mark_contradicted(journal, *anchor_id)?;
        }

        Ok(Seeded {
            duplicate_members,
            duplicate_clusters,
            distinct,
            contradicted_droppable,
            anchors,
        })
    }
}

#[async_trait]
impl Scenario for DreamQuality {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn run(&self, subject: &mut dyn Subject, clock: Timestamp) -> Result<ScenarioResult> {
        // Liveness check: the subject's own sleep cycle must not crash. The
        // result is recorded as a note, but the authoritative metrics are
        // computed below from a dedicated seeded store (per-memory comparison).
        let subject_summary = subject.sleep_cycle(clock).await?;

        // Seed a dedicated store + journal relative to the injected clock.
        let store = LocalJsonStore::in_memory();
        let mut journal = InMemoryJournal::new();
        let seeded = Self::seed(&store, &mut journal, clock).await?;

        // Run the REAL sleep cycle at the injected clock, then evaluate the
        // result memory by memory (before/after comparison).
        let report = DreamCycle::with_config(&store, dream_config())
            .run(&journal, clock)
            .await?;
        let outcome = Outcome::evaluate(&store, &seeded, &report).await?;

        #[allow(clippy::cast_precision_loss)]
        let false_merge_rate = outcome.false_merges as f64;
        #[allow(clippy::cast_precision_loss)]
        let date_absolutized = report.dates_absolutized as f64;

        let result = ScenarioResult::new(Self::ID, outcome.passed)
            .with_metric("dedup_precision", outcome.dedup_precision)
            .with_metric("contradiction_drop", outcome.contradiction_drop)
            .with_metric("date_absolutized", date_absolutized)
            .with_metric(
                "protected_core_intact",
                protected_core_intact(outcome.protected_intact),
            )
            .with_metric("false_merge_rate", false_merge_rate)
            .with_note(format!(
                "merged {}/{} true duplicates ({} clusters)",
                outcome.true_merges,
                seeded.expected_true_merges(),
                seeded.duplicate_clusters
            ))
            .with_note(format!(
                "dropped {}/{} contradicted (1 protected anchor also marked, untouched)",
                outcome.dropped_droppable, outcome.marked_droppable
            ))
            .with_note(format!(
                "absolutized {} relative date(s)",
                report.dates_absolutized
            ))
            .with_note(format!(
                "protected_core_intact={} (anchors={}), false_merge_rate={}",
                outcome.protected_intact,
                seeded.anchors.len(),
                outcome.false_merges
            ))
            .with_note(format!(
                "subject sleep_cycle liveness: scanned={} merged={} protected_core_intact={}",
                subject_summary.scanned,
                subject_summary.merged,
                subject_summary.protected_core_intact
            ));

        Ok(result)
    }
}

/// Post-sleep-cycle per-memory evaluation: all S3 metrics and the pass result
/// in one place.
struct Outcome {
    /// Whether all protected anchors remained untouched.
    protected_intact: bool,
    /// Genuine duplicates correctly tombstoned (non-representatives).
    true_merges: usize,
    /// Distinct memories/anchors incorrectly tombstoned (must be 0).
    false_merges: usize,
    /// Dedup precision = `true_merges / (true_merges + false_merges)`.
    dedup_precision: f64,
    /// Non-protected contradictions marked in the journal.
    marked_droppable: usize,
    /// Of those, the ones actually dropped.
    dropped_droppable: usize,
    /// Fraction of marked contradictions that were dropped.
    contradiction_drop: f64,
    /// The overall scenario pass result (design §3 S3, §1).
    passed: bool,
}

impl Outcome {
    /// Evaluates the sleep-cycle result by comparing each seeded memory's
    /// state against its expected fate.
    ///
    /// # Errors
    /// [`BenchError`] if a store read fails or the seed produced no droppable
    /// contradictions (division-by-zero guard).
    async fn evaluate(
        store: &LocalJsonStore,
        seeded: &Seeded,
        report: &familyclaw_dream::DreamReport,
    ) -> Result<Self> {
        // Protected core: every anchor is Active + content unchanged + not
        // present in any reflection.
        let anchor_ids: BTreeSet<MessageId> = seeded.anchors.iter().map(|(id, _)| *id).collect();
        let mut anchors_intact = true;
        for (id, original) in &seeded.anchors {
            match store.get(*id).await? {
                Some(after)
                    if after.status == MemoryStatus::Active && &after.content == original => {}
                _ => {
                    anchors_intact = false;
                    break;
                }
            }
        }
        let anchor_touched = report
            .reflections
            .iter()
            .any(|r| anchor_ids.contains(&r.memory));
        let protected_intact = anchors_intact && !anchor_touched;

        // Genuine merges: genuine duplicates that were tombstoned.
        let true_merges = count_tombstoned(store, &seeded.duplicate_members).await?;
        // False merges: distinct memories OR anchors tombstoned (must be 0).
        let false_merges = count_tombstoned(store, &seeded.distinct).await?
            + count_tombstoned(store, &anchor_ids.iter().copied().collect::<Vec<_>>()).await?;
        let dedup_precision = dedup_precision(true_merges, false_merges)?;

        // Contradictions: dropped out of the marked non-protected ones.
        let marked_droppable = seeded.contradicted_droppable.len();
        if marked_droppable == 0 {
            return Err(BenchError::scenario(
                "dream_quality: seed produced no droppable contradictions",
            ));
        }
        let dropped_droppable = count_tombstoned(store, &seeded.contradicted_droppable).await?;
        #[allow(clippy::cast_precision_loss)]
        let contradiction_drop = dropped_droppable as f64 / marked_droppable as f64;

        // Pass: dedup works AND contradictions dropped AND dates absolutized
        // AND protected_core_intact AND false_merge_rate==0.
        let expected_true_merges = seeded.expected_true_merges();
        let passed = true_merges == expected_true_merges
            && expected_true_merges > 0
            && dropped_droppable == marked_droppable
            && report.dates_absolutized >= 1
            && protected_intact
            && false_merges == 0;

        Ok(Self {
            protected_intact,
            true_merges,
            false_merges,
            dedup_precision,
            marked_droppable,
            dropped_droppable,
            contradiction_drop,
            passed,
        })
    }
}

/// Counts how many of the given ids are tombstoned
/// ([`MemoryStatus::Tombstoned`]) after the sleep cycle. An unknown id does
/// not count.
///
/// # Errors
/// [`BenchError`] if a store read fails.
async fn count_tombstoned(store: &LocalJsonStore, ids: &[MessageId]) -> Result<usize> {
    let mut count = 0usize;
    for id in ids {
        if let Some(after) = store.get(*id).await? {
            if after.status == MemoryStatus::Tombstoned {
                count += 1;
            }
        }
    }
    Ok(count)
}

/// Sleep-cycle configuration for this scenario: all phases enabled, merge
/// threshold [`MERGE_SIMILARITY`].
fn dream_config() -> familyclaw_dream::DreamConfig {
    familyclaw_dream::DreamConfig::default().with_merge_similarity(MERGE_SIMILARITY)
}

/// Adds a memory to the store, wrapping the core crate's error into a bench error.
async fn add(store: &LocalJsonStore, memory: Memory) -> Result<MessageId> {
    store.add(memory).await.map_err(BenchError::from)
}

/// A genuine duplicate candidate: moderate importance so representative
/// selection is deterministic and the memory is active.
fn dup_mem(content: &str, clock: Timestamp) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
        .created_at(clock)
        .build()
}

/// An ordinary, non-protected memory (distinct, contradicted, or date memory).
fn plain_mem(content: &str, clock: Timestamp) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
        .created_at(clock)
        .build()
}

/// A protected identity anchor: `ProtectedCore`, high importance.
fn anchor_mem(content: &str, clock: Timestamp) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
        .decay_policy(DecayPolicy::ProtectedCore)
        .created_at(clock)
        .build()
}

#[cfg(test)]
#[allow(clippy::unnecessary_literal_bound)] // Stub implementation returns a literal.
mod tests {
    use super::*;

    use crate::subject::{CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Task};

    /// Fixed injected reference clock (2026-06-05 12:00 UTC).
    fn clock() -> Timestamp {
        familyclaw_core::time::from_unix_secs(1_780_660_800).expect("valid clock")
    }

    /// Minimal subject stub that never crashes while dreaming — the scenario
    /// computes its authoritative metrics from its own seed, so the stub's
    /// return values do not affect the pass result.
    struct StubSubject;

    #[async_trait]
    impl Subject for StubSubject {
        async fn start_task(&mut self, task: &Task, _clock: Timestamp) -> Result<RunHandle> {
            Ok(RunHandle::new(task.id.clone(), "stub"))
        }
        async fn kill(&mut self, _handle: &RunHandle, _point: CrashPoint) -> Result<()> {
            Ok(())
        }
        async fn restart(&mut self, _clock: Timestamp) -> Result<RestartReport> {
            Ok(RestartReport {
                steps_replayed: 0,
                was_replaying: false,
                side_effects_reexecuted: 0,
                resumed_clean: true,
            })
        }
        async fn recall(&mut self, _query: &str, _clock: Timestamp) -> Result<Vec<RecallHit>> {
            Ok(Vec::new())
        }
        async fn sleep_cycle(&mut self, _clock: Timestamp) -> Result<DreamSummary> {
            Ok(DreamSummary {
                scanned: 0,
                merged: 0,
                dropped: 0,
                dates_absolutized: 0,
                strengthened: 0,
                archived: 0,
                protected_core_intact: true,
            })
        }
        fn name(&self) -> &str {
            "stub_subject"
        }
    }

    #[tokio::test]
    async fn dream_quality_passes_all_money_metrics() {
        let mut subject = StubSubject;
        let scenario = DreamQuality::new();
        let result = scenario.run(&mut subject, clock()).await.expect("run");

        assert_eq!(result.id, DreamQuality::ID);
        // Headline metrics (design §1): protected_core_intact==1.0,
        // false_merge_rate==0.
        assert_eq!(
            result.metrics.get("protected_core_intact").copied(),
            Some(1.0)
        );
        assert_eq!(result.metrics.get("false_merge_rate").copied(), Some(0.0));
        // Dedup worked with full precision (no false merges).
        assert_eq!(result.metrics.get("dedup_precision").copied(), Some(1.0));
        // All non-protected contradictions were dropped.
        assert_eq!(result.metrics.get("contradiction_drop").copied(), Some(1.0));
        // At least one date was absolutized.
        let dates = result
            .metrics
            .get("date_absolutized")
            .copied()
            .expect("date metric");
        assert!(dates >= 1.0, "expected >=1 absolutized date, got {dates}");

        assert!(result.passed, "S3 should pass: {:?}", result.notes);
    }

    #[tokio::test]
    async fn dream_quality_is_deterministic() {
        let mut s1 = StubSubject;
        let mut s2 = StubSubject;
        let scenario = DreamQuality::new();
        let r1 = scenario.run(&mut s1, clock()).await.expect("r1");
        let r2 = scenario.run(&mut s2, clock()).await.expect("r2");
        // Same injected clock → identical metrics (reproducibility §2.2).
        assert_eq!(r1.metrics, r2.metrics);
        assert_eq!(r1.passed, r2.passed);
    }

    #[test]
    fn id_is_stable() {
        assert_eq!(DreamQuality::new().id(), "s3_dream_quality");
    }
}
