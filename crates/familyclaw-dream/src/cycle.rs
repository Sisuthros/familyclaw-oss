//! Dream cycle engine: [`DreamCycle`].
//!
//! `DreamCycle` mirrors Anthropic's Dreaming model (design §2.3) as native
//! memory consolidation. It reads memories from a [`MemoryStore`]
//! implementation and conflict records from a durable [`Journal`], and runs
//! five phases:
//!
//! 1. **`merge_duplicates`** — merges near-identical memories into one
//!    representative (emotions + tags are unioned, the representative is
//!    strengthened, the rest are tombstoned).
//! 2. **`drop_contradicted`** — tombstones memories the durable journal has
//!    marked as outdated/contradicted.
//! 3. **`absolutize_dates`** — converts relative date words ("yesterday")
//!    to absolute ISO dates.
//! 4. **`consolidate`** — high-importance memories are strengthened,
//!    low-retention (R < threshold) memories are archived.
//! 5. produces a [`DreamReport`] to which all phases record their reflections.
//!
//! Phases run in a fixed order so the result is deterministic and
//! reproducible (same input ⇒ same report).

use std::collections::BTreeSet;

use familyclaw_core::{MessageId, Result, Timestamp};
use familyclaw_durable::Journal;
use familyclaw_memory::{Memory, MemoryStatus, MemoryStore};

use crate::config::DreamConfig;
use crate::contradiction::contradicted_ids;
use crate::dates::absolutize;
use crate::report::{DreamReport, Reflection, ReflectionKind};
use crate::similarity::is_near_duplicate;

/// The executor for a single dream cycle.
///
/// Holds a reference to the memory store and the configuration. The cycle
/// itself is run with the [`DreamCycle::run`] method (which also needs the
/// durable journal for conflict data) or with
/// [`DreamCycle::run_without_journal`] when the conflict phase isn't needed.
///
/// `S: MemoryStore + Sync` — `Sync` is required because the
/// [`MemoryStore::is_empty`] default method requires it and the cycle reads
/// the store concurrently. `S: ?Sized` allows trait objects (`dyn
/// MemoryStore`, `Arc<dyn MemoryStore>`, etc.).
#[derive(Debug)]
pub struct DreamCycle<'a, S>
where
    S: MemoryStore + Sync + ?Sized,
{
    /// The memory store being consolidated.
    store: &'a S,
    /// Thresholds and toggles for the phases.
    config: DreamConfig,
}

impl<'a, S> DreamCycle<'a, S>
where
    S: MemoryStore + Sync + ?Sized,
{
    /// Creates a dream cycle with the default configuration.
    #[must_use]
    pub fn new(store: &'a S) -> Self {
        Self {
            store,
            config: DreamConfig::default(),
        }
    }

    /// Creates a dream cycle with the given configuration.
    #[must_use]
    pub fn with_config(store: &'a S, config: DreamConfig) -> Self {
        Self { store, config }
    }

    /// Returns the configuration in use.
    #[must_use]
    pub fn config(&self) -> DreamConfig {
        self.config
    }

    /// Runs the full dream cycle at instant `at`, reading conflicts from `journal`.
    ///
    /// Phases run in order: merge → drop contradicted → absolutize dates →
    /// consolidate. Each phase can be toggled off in [`DreamConfig`].
    ///
    /// # Errors
    /// [`familyclaw_core::FamilyClawError`] if the memory store fails, or a
    /// durable journal read error translated into
    /// [`familyclaw_core::FamilyClawError::Memory`].
    pub async fn run(
        &self,
        journal: &(dyn Journal + Send + Sync),
        at: Timestamp,
    ) -> Result<DreamReport> {
        let contradicted = if self.config.drop_contradicted {
            contradicted_ids(journal)
                .map_err(|e| familyclaw_core::FamilyClawError::memory(e.to_string()))?
        } else {
            BTreeSet::new()
        };
        self.run_inner(&contradicted, at).await
    }

    /// Runs the dream cycle without a durable journal (the conflict phase
    /// is skipped regardless of configuration).
    ///
    /// # Errors
    /// [`familyclaw_core::FamilyClawError`] if the memory store fails.
    pub async fn run_without_journal(&self, at: Timestamp) -> Result<DreamReport> {
        self.run_inner(&BTreeSet::new(), at).await
    }

    /// Shared run path: takes the contradicted ids in as a ready-made set.
    async fn run_inner(
        &self,
        contradicted: &BTreeSet<MessageId>,
        at: Timestamp,
    ) -> Result<DreamReport> {
        let mut report = DreamReport::new(at);
        report.scanned = self.store.len().await?;

        if self.config.merge_duplicates {
            self.merge_duplicates(&mut report, at).await?;
        }
        if self.config.drop_contradicted {
            self.drop_contradicted(&mut report, contradicted).await?;
        }
        if self.config.absolutize_dates {
            self.absolutize_dates(&mut report, at).await?;
        }
        if self.config.consolidate {
            self.consolidate(&mut report, at).await?;
        }

        Ok(report)
    }

    /// Phase 1: merge near-identical memories.
    ///
    /// Groups retrievable memories into greedy clusters according to the
    /// similarity threshold ([`DreamConfig::merge_similarity`]). From each
    /// cluster of ≥ 2 members, the strongest representative is chosen,
    /// which is strengthened and receives the union of the others' tags +
    /// emotions; the rest are tombstoned.
    async fn merge_duplicates(&self, report: &mut DreamReport, at: Timestamp) -> Result<()> {
        let mut candidates: Vec<Memory> = self
            .store
            .all()
            .await?
            .into_iter()
            .filter(Memory::is_retrievable)
            .collect();
        // Deterministic starting order: oldest first, id breaks ties.
        candidates.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });

        let mut consumed: BTreeSet<MessageId> = BTreeSet::new();

        for i in 0..candidates.len() {
            let base_id = candidates[i].id;
            if consumed.contains(&base_id) {
                continue;
            }
            // Collect the duplicates belonging to this representative.
            let mut group: Vec<usize> = Vec::new();
            for (j, other) in candidates.iter().enumerate().skip(i + 1) {
                if consumed.contains(&other.id) {
                    continue;
                }
                if is_near_duplicate(
                    &candidates[i].content,
                    &other.content,
                    self.config.merge_similarity,
                ) {
                    group.push(j);
                }
            }
            if group.is_empty() {
                continue;
            }

            // Choose the strongest representative from the whole cluster (base + group).
            let mut cluster: Vec<usize> = std::iter::once(i).chain(group.iter().copied()).collect();
            cluster.sort_by(|&x, &y| representative_order(&candidates[x], &candidates[y]));
            let rep_idx = cluster[0];
            let rep_id = candidates[rep_idx].id;

            // Union tags + emotions into the representative and strengthen it.
            let mut rep = candidates[rep_idx].clone();
            for &idx in &cluster {
                if idx == rep_idx {
                    continue;
                }
                merge_metadata_into(&mut rep, &candidates[idx]);
            }
            rep.reinforce(at);
            self.store.update(rep).await?;

            // Tombstone the other cluster members.
            for &idx in &cluster {
                let id = candidates[idx].id;
                consumed.insert(id);
                if id == rep_id {
                    continue;
                }
                // CRITICAL INVARIANT (design §3 S3): the protected core
                // (λ=0, ProtectedCore) must NEVER be tombstoned — not even
                // as a non-representative during the merge phase. This
                // mirrors `Memory::tombstone()`'s refusal (memory.rs).
                // `representative_order` already favors an already-protected
                // memory as the representative, but if the cluster has >1
                // protected member (only one can be the representative), the
                // other protected members remain active and unchanged here.
                if candidates[idx].decay_policy.is_protected() {
                    continue;
                }
                self.store.set_status(id, MemoryStatus::Tombstoned).await?;
                report.record(Reflection::new(
                    ReflectionKind::Merged,
                    rep_id,
                    format!("merged duplicate {id} into {rep_id}"),
                ));
            }
        }
        Ok(())
    }

    /// Phase 2: drop memories the durable journal has marked as contradicted.
    async fn drop_contradicted(
        &self,
        report: &mut DreamReport,
        contradicted: &BTreeSet<MessageId>,
    ) -> Result<()> {
        for &id in contradicted {
            let Some(memory) = self.store.get(id).await? else {
                continue; // already removed or unknown id
            };
            // The protected core is never tombstoned, nor is an already-tombstoned memory.
            if memory.decay_policy.is_protected() || memory.status == MemoryStatus::Tombstoned {
                continue;
            }
            self.store.set_status(id, MemoryStatus::Tombstoned).await?;
            report.record(Reflection::new(
                ReflectionKind::Dropped,
                id,
                "dropped contradicted/outdated memory",
            ));
        }
        Ok(())
    }

    /// Phase 3: absolutize relative date words in memory content.
    async fn absolutize_dates(&self, report: &mut DreamReport, at: Timestamp) -> Result<()> {
        let memories = self.store.all().await?;
        for memory in memories {
            if !memory.is_retrievable() {
                continue;
            }
            let result = absolutize(&memory.content, at);
            if result.changed() {
                let id = memory.id;
                let mut updated = memory;
                updated.content = result.text;
                self.store.update(updated).await?;
                report.record(Reflection::new(
                    ReflectionKind::DateAbsolutized,
                    id,
                    format!("absolutized {} relative date(s)", result.replacements),
                ));
            }
        }
        Ok(())
    }

    /// Phase 4: strengthen important memories, archive low-retention memories.
    ///
    /// Strengthening and archiving are mutually exclusive per memory: an
    /// important memory is strengthened (and therefore not archived), a
    /// low-retention one is archived. The protected core is never touched
    /// by either.
    async fn consolidate(&self, report: &mut DreamReport, at: Timestamp) -> Result<()> {
        let memories = self.store.all().await?;
        for memory in memories {
            if memory.decay_policy.is_protected() {
                continue;
            }
            let id = memory.id;

            // Strengthen important active memories.
            if memory.status == MemoryStatus::Active
                && memory.importance >= self.config.strengthen_above_importance
            {
                self.store.reinforce(id, at).await?;
                report.record(Reflection::new(
                    ReflectionKind::Strengthened,
                    id,
                    "strengthened high-importance memory",
                ));
                continue;
            }

            // Archive low-retention active memories.
            if memory.status == MemoryStatus::Active
                && memory.retention(at) < self.config.archive_below_retention
            {
                self.store.set_status(id, MemoryStatus::Archived).await?;
                report.record(Reflection::new(
                    ReflectionKind::Archived,
                    id,
                    "archived low-retention memory",
                ));
            }
        }
        Ok(())
    }
}

/// Comparison function for choosing the representative: strongest first.
///
/// Order: **protected core first** (`ProtectedCore` always wins, so an
/// identity anchor never ends up tombstoned as a non-representative) →
/// higher importance → more recent (`last_reinforced_at`) → smaller id
/// (deterministic tiebreaker).
fn representative_order(a: &Memory, b: &Memory) -> std::cmp::Ordering {
    // A protected core is chosen as the representative regardless of its
    // importance value: this way a non-protected near-duplicate can never
    // displace it and cause the anchor to be tombstoned (design §3 S3:
    // protected_core_intact == 1.0).
    let a_protected = a.decay_policy.is_protected();
    let b_protected = b.decay_policy.is_protected();
    b_protected
        .cmp(&a_protected)
        .then_with(|| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| b.last_reinforced_at.cmp(&a.last_reinforced_at))
        .then_with(|| a.id.cmp(&b.id))
}

/// Merges the source's tags and emotions into the representative (union,
/// preserving order and removing duplicates).
fn merge_metadata_into(rep: &mut Memory, source: &Memory) {
    for tag in &source.tags {
        if !rep.tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
            rep.tags.push(tag.clone());
        }
    }
    for emotion in &source.emotions {
        if !rep.emotions.contains(emotion) {
            rep.emotions.push(*emotion);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};
    use familyclaw_core::time;
    use familyclaw_durable::InMemoryJournal;
    use familyclaw_memory::{DecayPolicy, Dimension, ImportanceFactors, LocalJsonStore};

    use crate::contradiction::mark_contradicted;

    /// Fixed reference instant: 2026-06-04 12:00 UTC.
    fn at() -> Timestamp {
        Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0)
            .single()
            .expect("valid instant")
    }

    fn mem(content: &str, importance: f32) -> Memory {
        Memory::builder(content)
            .factors(ImportanceFactors::new(importance, 0.0, 0.0, 0.0))
            .build()
    }

    // --- Phase 1: merge_duplicates -------------------------------------------

    #[tokio::test]
    async fn merge_combines_near_duplicates() {
        let store = LocalJsonStore::in_memory();
        // Two near-identical memories (only one word differs) + one distinct.
        let a = Memory::builder("the family shipped the bridge today")
            .factors(ImportanceFactors::new(0.3, 0.0, 0.0, 0.0))
            .tags(["work".to_string()])
            .build();
        let b = Memory::builder("the family shipped the bridge")
            .factors(ImportanceFactors::new(0.6, 0.0, 0.0, 0.0))
            .tags(["milestone".to_string()])
            .emotions([Dimension::Pride])
            .build();
        let c = mem("completely unrelated cooking recipe", 0.4);
        store.add(a).await.expect("a");
        let b_id = store.add(b).await.expect("b");
        store.add(c).await.expect("c");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .with_merge_similarity(0.7)
                // isolate this phase from the others
                .dropping_contradicted(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");

        assert_eq!(report.merged, 1, "one duplicate should be merged");
        // Representative = b (higher importance) — stays active and received a's tags.
        let rep = store.get(b_id).await.expect("g").expect("p");
        assert_eq!(rep.status, MemoryStatus::Active);
        assert!(rep.tags.iter().any(|t| t == "work"));
        assert!(rep.tags.iter().any(|t| t == "milestone"));
        assert!(rep.emotions.contains(&Dimension::Pride));
        assert!(
            rep.reinforcement_count >= 1,
            "the representative was strengthened"
        );

        // Exactly one tombstoned, one untouched (c).
        let all = store.all().await.expect("all");
        let tombstoned = all
            .iter()
            .filter(|m| m.status == MemoryStatus::Tombstoned)
            .count();
        assert_eq!(tombstoned, 1);
    }

    /// Regression (red-team `dream-corruption`, 2026-06-05): a protected
    /// identity anchor must NOT be tombstoned during the merge phase, even
    /// when the cluster contains a higher-importance, non-protected
    /// near-duplicate. Previously `merge_duplicates` called
    /// `set_status(Tombstoned)` directly, bypassing the `is_protected()`
    /// guard (unlike `drop_contradicted` and `consolidate`) → the anchor was
    /// tombstoned as a non-representative and `protected_core_intact` broke.
    /// Fix: the protected core is always chosen as the representative AND
    /// a protected memory is never tombstoned in the merge loop.
    #[tokio::test]
    async fn merge_never_tombstones_protected_core_as_nonrepresentative() {
        let store = LocalJsonStore::in_memory();
        // ProtectedCore, LOWER importance.
        let anchor = store
            .add(
                Memory::builder("i am part of this family and always will be")
                    .factors(ImportanceFactors::new(0.40, 0.40, 0.0, 0.0))
                    .decay_policy(DecayPolicy::ProtectedCore)
                    .build(),
            )
            .await
            .expect("anchor");
        // Non-protected, HIGHER importance, lexically near-identical
        // (Jaccard ≈ 0.857 ≥ 0.85 default threshold) → clusters with the anchor.
        let dup = store
            .add(
                Memory::builder("i am part of this family and always will be forever")
                    .factors(ImportanceFactors::new(0.95, 0.95, 0.0, 0.0))
                    .build(),
            )
            .await
            .expect("dup");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .dropping_contradicted(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let _ = cycle.run_without_journal(at()).await.expect("run");

        // The anchor must remain active and unchanged in content.
        let anchor_after = store.get(anchor).await.expect("g").expect("p");
        assert_eq!(
            anchor_after.status,
            MemoryStatus::Active,
            "protected anchor was tombstoned during the merge phase as a non-representative"
        );
        assert_eq!(
            anchor_after.content, "i am part of this family and always will be",
            "the protected anchor's content changed"
        );
        // The protected core is chosen as the representative → the non-protected duplicate is lost.
        assert_eq!(
            store.get(dup).await.expect("g").expect("p").status,
            MemoryStatus::Tombstoned,
            "the non-protected near-duplicate should be lost to the protected representative"
        );
    }

    #[tokio::test]
    async fn merge_leaves_distinct_memories_untouched() {
        let store = LocalJsonStore::in_memory();
        store
            .add(mem("rust async runtime design", 0.5))
            .await
            .expect("a");
        store
            .add(mem("python web framework tutorial", 0.5))
            .await
            .expect("b");
        store
            .add(mem("a song about the ocean waves", 0.5))
            .await
            .expect("c");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .dropping_contradicted(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert_eq!(report.merged, 0);
        let active = store
            .all()
            .await
            .expect("all")
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .count();
        assert_eq!(active, 3);
    }

    #[tokio::test]
    async fn merge_three_way_cluster_keeps_one() {
        let store = LocalJsonStore::in_memory();
        store
            .add(mem("agent_a is in city a", 0.2))
            .await
            .expect("a");
        store
            .add(mem("agent_a is in city a now", 0.9))
            .await
            .expect("b");
        store
            .add(mem("agent_a is in city a today", 0.3))
            .await
            .expect("c");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .with_merge_similarity(0.6)
                .dropping_contradicted(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert_eq!(report.merged, 2, "kolmesta jää yksi → kaksi yhdistyy");
        let active = store
            .all()
            .await
            .expect("all")
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .count();
        assert_eq!(active, 1);
    }

    // --- Phase 2: drop_contradicted ------------------------------------------

    #[tokio::test]
    async fn drop_contradicted_tombstones_marked() {
        let store = LocalJsonStore::in_memory();
        let stale = store
            .add(mem("agent_a is in city a", 0.5))
            .await
            .expect("stale");
        let fresh = store.add(mem("the sky is blue", 0.5)).await.expect("fresh");

        let mut journal = InMemoryJournal::new();
        mark_contradicted(&mut journal, stale).expect("mark");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run(&journal, at()).await.expect("run");

        assert_eq!(report.dropped, 1);
        assert_eq!(
            store.get(stale).await.expect("g").expect("p").status,
            MemoryStatus::Tombstoned
        );
        assert_eq!(
            store.get(fresh).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
    }

    #[tokio::test]
    async fn drop_contradicted_never_drops_protected_core() {
        let store = LocalJsonStore::in_memory();
        let anchor = store
            .add(
                Memory::builder("i am part of this family")
                    .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
                    .decay_policy(DecayPolicy::ProtectedCore)
                    .build(),
            )
            .await
            .expect("anchor");

        let mut journal = InMemoryJournal::new();
        mark_contradicted(&mut journal, anchor).expect("mark");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run(&journal, at()).await.expect("run");
        assert_eq!(report.dropped, 0, "suojattua ydintä ei saa pudottaa");
        assert_eq!(
            store.get(anchor).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
    }

    #[tokio::test]
    async fn drop_contradicted_ignores_unknown_ids() {
        let store = LocalJsonStore::in_memory();
        store.add(mem("real memory", 0.5)).await.expect("real");

        let mut journal = InMemoryJournal::new();
        mark_contradicted(&mut journal, MessageId::new()).expect("mark ghost");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run(&journal, at()).await.expect("run");
        assert_eq!(report.dropped, 0);
    }

    // --- Phase 3: absolutize_dates -------------------------------------------

    #[tokio::test]
    async fn absolutize_rewrites_relative_dates() {
        let store = LocalJsonStore::in_memory();
        let id = store
            .add(mem("agent_a left eilen for the airport", 0.5))
            .await
            .expect("add");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .dropping_contradicted(false)
                .consolidating(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");

        assert_eq!(report.dates_absolutized, 1);
        let updated = store.get(id).await.expect("g").expect("p");
        assert!(
            updated.content.contains("eilen (2026-06-03)"),
            "sai: {}",
            updated.content
        );
    }

    #[tokio::test]
    async fn absolutize_is_idempotent_across_runs() {
        let store = LocalJsonStore::in_memory();
        store.add(mem("shipped tomorrow", 0.5)).await.expect("add");
        let cfg = DreamConfig::default()
            .merging(false)
            .dropping_contradicted(false)
            .consolidating(false);

        let cycle = DreamCycle::with_config(&store, cfg);
        let first = cycle.run_without_journal(at()).await.expect("first");
        assert_eq!(first.dates_absolutized, 1);
        let second = cycle.run_without_journal(at()).await.expect("second");
        assert_eq!(second.dates_absolutized, 0, "toinen uni ei lisää päiviä");
    }

    // --- Phase 4: consolidate ------------------------------------------------

    #[tokio::test]
    async fn consolidate_strengthens_important_memories() {
        let store = LocalJsonStore::in_memory();
        // importance = 0.9·0.45 = 0.405? — push it above the 0.6 threshold via identity.
        let id = store
            .add(
                Memory::builder("a deeply important milestone")
                    .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
                    .build(),
            )
            .await
            .expect("add");
        let before = store
            .get(id)
            .await
            .expect("g")
            .expect("p")
            .reinforcement_count;

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .dropping_contradicted(false)
                .absolutizing_dates(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert_eq!(report.strengthened, 1);
        let after = store.get(id).await.expect("g").expect("p");
        assert_eq!(after.reinforcement_count, before + 1);
    }

    #[tokio::test]
    async fn consolidate_archives_low_retention_memories() {
        let store = LocalJsonStore::in_memory();
        // Low importance + fast decay + old → very low retention.
        let created = at() - Duration::days(60);
        let id = store
            .add(
                Memory::builder("a fleeting trivial observation")
                    .factors(ImportanceFactors::new(0.02, 0.0, 0.0, 0.0))
                    .decay_policy(DecayPolicy::Fast)
                    .created_at(created)
                    .build(),
            )
            .await
            .expect("add");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .dropping_contradicted(false)
                .absolutizing_dates(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert_eq!(report.archived, 1);
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Archived
        );
    }

    #[tokio::test]
    async fn consolidate_never_touches_protected_core() {
        let store = LocalJsonStore::in_memory();
        let created = at() - Duration::days(10_000);
        let id = store
            .add(
                Memory::builder("identity anchor")
                    .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
                    .decay_policy(DecayPolicy::ProtectedCore)
                    .created_at(created)
                    .build(),
            )
            .await
            .expect("add");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .dropping_contradicted(false)
                .absolutizing_dates(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert_eq!(report.strengthened, 0);
        assert_eq!(report.archived, 0);
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
    }

    // --- Full cycle + edge cases --------------------------------------------

    #[tokio::test]
    async fn full_cycle_runs_all_phases() {
        let store = LocalJsonStore::in_memory();
        // duplicates
        store
            .add(mem("we shipped the release", 0.3))
            .await
            .expect("d1");
        let keep = store
            .add(mem("we shipped the release", 0.8))
            .await
            .expect("d2");
        // conflict
        let stale = store
            .add(mem("server is in frankfurt", 0.5))
            .await
            .expect("stale");
        // relative date
        store
            .add(mem("meeting happened eilen", 0.4))
            .await
            .expect("date");
        // low retention
        let created = at() - Duration::days(90);
        store
            .add(
                Memory::builder("trivial note")
                    .factors(ImportanceFactors::new(0.02, 0.0, 0.0, 0.0))
                    .decay_policy(DecayPolicy::Fast)
                    .created_at(created)
                    .build(),
            )
            .await
            .expect("trivial");

        let mut journal = InMemoryJournal::new();
        mark_contradicted(&mut journal, stale).expect("mark");

        let cycle =
            DreamCycle::with_config(&store, DreamConfig::default().with_merge_similarity(0.9));
        let report = cycle.run(&journal, at()).await.expect("run");

        assert_eq!(report.scanned, 5);
        assert_eq!(report.merged, 1);
        assert_eq!(report.dropped, 1);
        assert_eq!(report.dates_absolutized, 1);
        assert!(report.made_changes());
        // Sum of reflections = sum of counters.
        assert_eq!(report.reflections.len(), report.total_actions());

        // The merged representative survived.
        assert_eq!(
            store.get(keep).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
    }

    #[tokio::test]
    async fn empty_store_yields_no_changes() {
        let store = LocalJsonStore::in_memory();
        let cycle = DreamCycle::new(&store);
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert_eq!(report.scanned, 0);
        assert!(!report.made_changes());
        assert!(report.reflections.is_empty());
        assert!(report.ran_at.is_some());
    }

    #[tokio::test]
    async fn disabled_phases_do_nothing() {
        let store = LocalJsonStore::in_memory();
        store
            .add(mem("we shipped the release", 0.3))
            .await
            .expect("d1");
        store
            .add(mem("we shipped the release", 0.8))
            .await
            .expect("d2");

        let cfg = DreamConfig::default()
            .merging(false)
            .dropping_contradicted(false)
            .absolutizing_dates(false)
            .consolidating(false);
        let cycle = DreamCycle::with_config(&store, cfg);
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert!(!report.made_changes());
        assert_eq!(report.scanned, 2);
    }

    #[tokio::test]
    async fn config_accessor_returns_configured_value() {
        let store = LocalJsonStore::in_memory();
        let cfg = DreamConfig::default().with_merge_similarity(0.42);
        let cycle = DreamCycle::with_config(&store, cfg);
        assert!((cycle.config().merge_similarity - 0.42).abs() < 1e-6);
    }

    #[tokio::test]
    async fn run_without_journal_skips_contradiction_phase() {
        let store = LocalJsonStore::in_memory();
        let id = store
            .add(mem("would be contradicted", 0.5))
            .await
            .expect("add");
        // Even though drop_contradicted is on, nothing is dropped without a journal.
        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run_without_journal(time::now()).await.expect("run");
        assert_eq!(report.dropped, 0);
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
    }

    #[test]
    fn representative_order_prefers_higher_importance() {
        let strong = mem("x", 0.9);
        let weak = mem("x", 0.1);
        assert_eq!(
            representative_order(&strong, &weak),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn representative_order_prefers_protected_core_over_higher_importance() {
        // A protected core (low importance) beats a non-protected one (high
        // importance) — prevents the anchor from being tombstoned as a non-representative.
        let protected = Memory::builder("x")
            .factors(ImportanceFactors::new(0.1, 0.1, 0.0, 0.0))
            .decay_policy(DecayPolicy::ProtectedCore)
            .build();
        let strong = mem("x", 0.9);
        assert_eq!(
            representative_order(&protected, &strong),
            std::cmp::Ordering::Less,
            "ProtectedCore on järjestyksessä ensin (edustaja)"
        );
        assert_eq!(
            representative_order(&strong, &protected),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn merge_metadata_unions_without_duplicates() {
        let mut rep = Memory::builder("base")
            .tags(["a".to_string()])
            .emotions([Dimension::Joy])
            .build();
        let src = Memory::builder("other")
            .tags(["A".to_string(), "b".to_string()])
            .emotions([Dimension::Joy, Dimension::Hope])
            .build();
        merge_metadata_into(&mut rep, &src);
        // "A" is already present (case-insensitive) → not added; "b" is added.
        assert_eq!(rep.tags, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(rep.emotions, vec![Dimension::Joy, Dimension::Hope]);
    }
}
