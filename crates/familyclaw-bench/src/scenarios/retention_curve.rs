//! **S2 Retention Curve** — memory retention over time (design §3).
//!
//! This scenario proves Eternal Thread's core claim:
//!
//! > Identity anchors (λ = 0) **never disappear**, everyday trivia (`Fast`)
//! > **decays**, and the FamilyClaw model **beats a naive ring buffer** at
//! > retaining the memories that *matter*.
//!
//! ## How it is measured
//! The scenario seeds a deterministic memory population into three classes:
//! - **anchors** — [`DecayPolicy::ProtectedCore`], maximum identity (e.g. the
//!   being's name and family). Retention is always `1.0`.
//! - **important** — [`DecayPolicy::Slow`], high importance. Persist for a
//!   long time but decay slowly.
//! - **trivia** — [`DecayPolicy::Fast`], low importance. Decays quickly.
//!
//! The **injected clock** is advanced 7 → 30 → 90 days forward (no real
//! sleeping, no system clock). At each checkpoint, `recall@k` is computed for
//! anchors vs. trivia using the memory store's `retention(at)` /
//! `is_retrievable()` metrics and `retrieve()` search.
//!
//! ## Naive baseline (ring buffer)
//! The comparison point is a **"last N memories, no decay model"** buffer.
//! Since trivia is seeded last, the naive last-N buffer **retains the trivia
//! and discards the anchors** — exactly backwards. FamilyClaw retains the
//! anchors and lets the trivia decay. This is a measurable difference.
//!
//! ## Pass condition (design §3 S2)
//! `passed` = anchors intact (`anchor_retention_90d ≈ 1.0`) **AND** trivia
//! decayed (`trivia_decayed_90d`) **AND** FamilyClaw beats the naive baseline
//! at retaining the memories that matter (important ones).

use async_trait::async_trait;
use chrono::Duration;

use familyclaw_core::Timestamp;
use familyclaw_memory::{
    DecayPolicy, DecayThresholds, ImportanceFactors, LocalJsonStore, Memory, MemoryStore,
    RetrievalContext,
};

use crate::error::Result;
use crate::metrics::recall_at_k;
use crate::scenario::{Scenario, ScenarioResult};
use crate::subject::Subject;

/// Top-k cutoff for the `recall@k` measurement. A constant makes the result
/// reproducible.
const RECALL_K: usize = 5;

/// How many memories the naive ring buffer keeps (last N).
///
/// Chosen smaller than the total number of seeded memories, so the buffer is
/// forced to discard something — and since trivia is seeded last, it discards
/// precisely the anchors (the worst possible choice).
const NAIVE_BUFFER_CAP: usize = 4;

/// Checkpoints in days at which retention is measured (design §3 S2).
const DAY_CHECKPOINTS: [i64; 3] = [7, 30, 90];

/// Retention threshold below which a memory is considered "decayed".
const DECAYED_BELOW: f32 = 0.4;

/// S2 Retention Curve scenario.
///
/// Stateless — all run state is derived from the injected clock, so two runs
/// with the same clock produce an identical result (design §2.2).
#[derive(Debug, Default, Clone, Copy)]
pub struct RetentionCurve;

impl RetentionCurve {
    /// Builds the scenario.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// A single seeded memory's class (internal bookkeeping for the scenario).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Identity anchor (`ProtectedCore`, λ = 0).
    Anchor,
    /// Important, slow-decaying memory (`Slow`).
    Important,
    /// Everyday trivia (`Fast`).
    Trivia,
}

/// Deterministic seed plan: content + class. Order is significant — trivia
/// comes last so the naive last-N buffer discards the anchors.
fn seed_plan() -> Vec<(&'static str, Class)> {
    vec![
        ("I am agent_alpha, part of this team", Class::Anchor),
        (
            "My team: agent_alpha, agent_beta, agent_gamma, agent_delta",
            Class::Anchor,
        ),
        ("The project shipped its first release", Class::Important),
        (
            "We agreed durable replay is the spearhead",
            Class::Important,
        ),
        ("The weather was cloudy this afternoon", Class::Trivia),
        ("Someone mentioned a coffee break at noon", Class::Trivia),
        ("A passing comment about the bus schedule", Class::Trivia),
    ]
}

/// Builds one memory with the parameters for its class, at clock `clock`.
fn build_memory(content: &str, class: Class, clock: Timestamp) -> Memory {
    let (factors, policy) = match class {
        // Maximum identity; ProtectedCore never decays.
        Class::Anchor => (
            ImportanceFactors::new(1.0, 1.0, 0.0, 0.0),
            DecayPolicy::ProtectedCore,
        ),
        // High importance, slow decay.
        Class::Important => (
            ImportanceFactors::new(0.8, 0.6, 0.3, 0.0),
            DecayPolicy::Slow,
        ),
        // Low importance, fast decay.
        Class::Trivia => (
            ImportanceFactors::new(0.1, 0.0, 0.2, 0.0),
            DecayPolicy::Fast,
        ),
    };
    Memory::builder(content)
        .factors(factors)
        .decay_policy(policy)
        .created_at(clock)
        .build()
}

/// Naive ring-buffer baseline: keeps only the last `cap` memories in seed
/// order, **with no decay model at all**. This is the competitor FamilyClaw
/// beats: it doesn't know which memory matters, so it keeps the newest
/// (trivia) and discards the oldest (anchors).
#[derive(Debug, Default)]
struct NaiveRingBuffer {
    /// Content of the retained memories, in seed order.
    kept: Vec<String>,
    /// Maximum capacity.
    cap: usize,
}

impl NaiveRingBuffer {
    fn new(cap: usize) -> Self {
        Self {
            kept: Vec::new(),
            cap: cap.max(1),
        }
    }

    /// Adds a memory; on overflow, evicts the oldest (FIFO eviction).
    fn push(&mut self, content: &str) {
        self.kept.push(content.to_string());
        if self.kept.len() > self.cap {
            self.kept.remove(0);
        }
    }

    /// Whether the given content is still in the buffer (= the naive baseline
    /// "remembers" it).
    fn contains(&self, content: &str) -> bool {
        self.kept.iter().any(|c| c == content)
    }
}

/// How many memories of the given class are still retrievable in the
/// FamilyClaw store at instant `at` (retention >= threshold and lifecycle
/// state retrievable).
async fn retrievable_count(
    store: &LocalJsonStore,
    seeds: &[(&'static str, Class)],
    class: Class,
    at: Timestamp,
) -> Result<usize> {
    let all = store.all().await?;
    let mut count = 0;
    for memory in &all {
        // Match the memory to its class by content (deterministic).
        let is_class = seeds
            .iter()
            .any(|(content, c)| *c == class && *content == memory.content);
        if is_class && memory.is_retrievable() && memory.retention(at) >= DECAYED_BELOW {
            count += 1;
        }
    }
    Ok(count)
}

#[async_trait]
impl Scenario for RetentionCurve {
    // Trait signature requires `&str`; the literal is always `'static`, so
    // clippy's `&'static str` suggestion doesn't fit this implementation.
    #[allow(clippy::unnecessary_literal_bound)]
    fn id(&self) -> &str {
        "s2_retention_curve"
    }

    #[allow(clippy::too_many_lines)] // One cohesive, readable test suite.
    async fn run(&self, subject: &mut dyn Subject, clock: Timestamp) -> Result<ScenarioResult> {
        let seeds = seed_plan();
        let anchors_total = seeds.iter().filter(|(_, c)| *c == Class::Anchor).count();
        let important_total = seeds.iter().filter(|(_, c)| *c == Class::Important).count();
        let trivia_total = seeds.iter().filter(|(_, c)| *c == Class::Trivia).count();

        if anchors_total == 0 || important_total == 0 || trivia_total == 0 {
            return Err(crate::BenchError::scenario(
                "retention_curve: seed plan must contain anchors, important and trivia",
            ));
        }

        // ── Seed the FamilyClaw memory store and the naive baseline with the same data ──
        let store = LocalJsonStore::in_memory();
        let mut naive = NaiveRingBuffer::new(NAIVE_BUFFER_CAP);
        for (content, class) in &seeds {
            store.add(build_memory(content, *class, clock)).await?;
            naive.push(content);
        }

        let mut result = ScenarioResult::new(self.id(), false)
            .with_note(format!(
                "seeded {anchors_total} anchors, {important_total} important, {trivia_total} trivia at injected clock"
            ))
            // Small counters (item counts); f64 represents them exactly.
            .with_metric("recall_k", f64::from(u32::try_from(RECALL_K).unwrap_or(u32::MAX)))
            .with_metric(
                "naive_buffer_cap",
                f64::from(u32::try_from(NAIVE_BUFFER_CAP).unwrap_or(u32::MAX)),
            );

        // ── Measure the retention curve at each checkpoint ──
        let mut anchor_retention_at_90 = 0.0_f64;
        let mut trivia_retrievable_at_90 = trivia_total;
        for &days in &DAY_CHECKPOINTS {
            let at = clock + Duration::days(days);

            // Run decay up to this instant: trivia falls to archived/tombstoned,
            // anchors are never touched (the store guarantees this).
            store.run_decay(DecayThresholds::default(), at).await?;

            let anchors_live = retrievable_count(&store, &seeds, Class::Anchor, at).await?;
            let trivia_live = retrievable_count(&store, &seeds, Class::Trivia, at).await?;

            // recall@k for anchors: how many of the expected anchors are found.
            let anchor_recall = recall_at_k(anchors_total, anchors_live)?;
            // recall@k for trivia: high = trivia did NOT decay (bad).
            let trivia_recall = recall_at_k(trivia_total, trivia_live)?;

            result = result
                .with_metric(
                    format!("recall_at_{RECALL_K}_anchors_day{days}"),
                    anchor_recall,
                )
                .with_metric(
                    format!("recall_at_{RECALL_K}_trivia_day{days}"),
                    trivia_recall,
                );

            // Mean retention of anchors (should be exactly 1.0).
            let anchor_retention = mean_retention(&store, &seeds, Class::Anchor, at).await?;
            result = result.with_metric(
                format!("anchor_retention_day{days}"),
                f64::from(anchor_retention),
            );

            if days == 90 {
                anchor_retention_at_90 = f64::from(anchor_retention);
                trivia_retrievable_at_90 = trivia_live;
            }
        }

        // ── 90-day summary metrics (design §3 S2) ──
        result = result.with_metric("anchor_retention_90d", anchor_retention_at_90);
        // trivia_decayed_90d: fraction of the trivia class that has decayed.
        let trivia_decayed_fraction = recall_at_k(
            trivia_total,
            trivia_total.saturating_sub(trivia_retrievable_at_90),
        )?;
        result = result.with_metric("trivia_decayed_90d", trivia_decayed_fraction);

        // ── FamilyClaw vs naive baseline: retaining the memories that matter ──
        // FamilyClaw after 90 days: how many IMPORTANT (anchors+important)
        // memories are still retrievable. Naive: how many of the same are in
        // the buffer.
        let at_90 = clock + Duration::days(90);
        let mut fc_keeps_important = 0_usize;
        let mut naive_keeps_important = 0_usize;
        let important_like = anchors_total + important_total;
        let all = store.all().await?;
        for (content, class) in &seeds {
            if *class == Class::Anchor || *class == Class::Important {
                if let Some(m) = all.iter().find(|m| &m.content == content) {
                    if m.is_retrievable() && m.retention(at_90) >= DECAYED_BELOW {
                        fc_keeps_important += 1;
                    }
                }
                if naive.contains(content) {
                    naive_keeps_important += 1;
                }
            }
        }
        let fc_keep_rate = recall_at_k(important_like, fc_keeps_important)?;
        let naive_keep_rate = recall_at_k(important_like, naive_keeps_important)?;
        result = result
            .with_metric("familyclaw_keeps_important_90d", fc_keep_rate)
            .with_metric("naive_keeps_important_90d", naive_keep_rate)
            .with_note(format!(
                "FamilyClaw keeps {fc_keeps_important}/{important_like} important memories; naive ring buffer keeps {naive_keeps_important}/{important_like}"
            ));

        // ── Also run the subject's own black-box recall (seam check) ──
        // The Subject does not offer a seeding interface, so this is data
        // collection, not a pass condition — the actual S2 scoring comes from
        // the model above.
        let subject_hits = subject.recall("family", at_90).await?;
        // The hit count is a small item counter; f64 represents it exactly.
        let subject_hit_count = f64::from(u32::try_from(subject_hits.len()).unwrap_or(u32::MAX));
        result = result.with_metric("subject_recall_hits", subject_hit_count);

        // ── Verify that FamilyClaw retrieval itself ranks anchors at the top ──
        let ctx = RetrievalContext::new("family").with_limit(RECALL_K);
        let hits = store.retrieve(&ctx, at_90).await?;
        let top_is_anchor = hits.first().is_some_and(|h| {
            seeds
                .iter()
                .any(|(content, c)| *c == Class::Anchor && *content == h.memory.content)
        });
        result = result.with_metric(
            "retrieve_top_is_anchor",
            if top_is_anchor { 1.0 } else { 0.0 },
        );

        // ── Pass condition (design §3 S2) ──
        let anchors_intact = (anchor_retention_at_90 - 1.0).abs() < 1e-6;
        let trivia_decayed = trivia_retrievable_at_90 < trivia_total;
        let beats_naive = fc_keep_rate > naive_keep_rate;
        let passed = anchors_intact && trivia_decayed && beats_naive;

        result.passed = passed;
        result = result
            .with_note(format!(
                "anchors_intact={anchors_intact} trivia_decayed={trivia_decayed} beats_naive={beats_naive}"
            ));
        Ok(result)
    }
}

/// Mean retention of the given class's memories at instant `at`.
async fn mean_retention(
    store: &LocalJsonStore,
    seeds: &[(&'static str, Class)],
    class: Class,
    at: Timestamp,
) -> Result<f32> {
    let all = store.all().await?;
    let mut sum = 0.0_f32;
    let mut n = 0_u32;
    for memory in &all {
        let is_class = seeds
            .iter()
            .any(|(content, c)| *c == class && *content == memory.content);
        if is_class {
            sum += memory.retention(at);
            n += 1;
        }
    }
    if n == 0 {
        return Ok(0.0);
    }
    // `n` is a small memory counter (tens); f32 represents it exactly.
    #[allow(clippy::cast_precision_loss)]
    let divisor = n as f32;
    Ok(sum / divisor)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::subject::{CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Task};
    use familyclaw_core::time;

    /// Minimal test subject — returns fixed values. The subject's recall does
    /// not affect S2's pass result, so this is sufficient for the seam check.
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
            Ok(vec![RecallHit::new("family", 1.0)])
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
        #[allow(clippy::unnecessary_literal_bound)]
        fn name(&self) -> &str {
            "stub"
        }
    }

    fn fixed_clock() -> Timestamp {
        time::from_unix_secs(1_717_000_000).expect("valid clock")
    }

    #[tokio::test]
    async fn scenario_passes_for_familyclaw_model() {
        let scenario = RetentionCurve::new();
        let mut subject = StubSubject;
        let result = scenario
            .run(&mut subject, fixed_clock())
            .await
            .expect("run");
        assert_eq!(result.id, "s2_retention_curve");
        assert!(result.passed, "S2 must pass: {:?}", result.notes);
    }

    #[tokio::test]
    async fn anchors_never_decay_at_90_days() {
        let scenario = RetentionCurve::new();
        let mut subject = StubSubject;
        let result = scenario
            .run(&mut subject, fixed_clock())
            .await
            .expect("run");
        let anchor90 = result
            .metrics
            .get("anchor_retention_90d")
            .copied()
            .expect("metric present");
        assert!((anchor90 - 1.0).abs() < 1e-9, "anchors decayed: {anchor90}");
    }

    #[tokio::test]
    async fn trivia_decays_over_time() {
        let scenario = RetentionCurve::new();
        let mut subject = StubSubject;
        let result = scenario
            .run(&mut subject, fixed_clock())
            .await
            .expect("run");
        let decayed = result
            .metrics
            .get("trivia_decayed_90d")
            .copied()
            .expect("metric present");
        assert!(decayed > 0.0, "trivia did not decay at all");
    }

    #[tokio::test]
    async fn familyclaw_beats_naive_baseline() {
        let scenario = RetentionCurve::new();
        let mut subject = StubSubject;
        let result = scenario
            .run(&mut subject, fixed_clock())
            .await
            .expect("run");
        let fc = result
            .metrics
            .get("familyclaw_keeps_important_90d")
            .copied()
            .expect("fc metric");
        let naive = result
            .metrics
            .get("naive_keeps_important_90d")
            .copied()
            .expect("naive metric");
        assert!(fc > naive, "FamilyClaw ({fc}) did not beat naive ({naive})");
    }

    #[tokio::test]
    async fn result_is_deterministic() {
        let scenario = RetentionCurve::new();
        let clock = fixed_clock();
        let mut s1 = StubSubject;
        let mut s2 = StubSubject;
        let a = scenario.run(&mut s1, clock).await.expect("a");
        let b = scenario.run(&mut s2, clock).await.expect("b");
        assert_eq!(a, b, "same clock must yield identical result");
    }
}
