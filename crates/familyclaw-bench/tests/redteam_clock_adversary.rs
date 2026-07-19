//! RED-TEAM: a clock adversary against the sleep cycle and decay.
//!
//! Attack claim we try to BREAK:
//! > "Dream cleans memory without ever harming protected identity anchors,
//! >  even when the injected wall clock runs BACKWARDS or repeats timestamps."
//!
//! Concrete attacks (all run REAL code, not mocks):
//!
//! 1. `dream_with_backwards_clock_*` — feed [`DreamCycle::run`] an `at` that
//!    is EARLIER than the memories' `created_at`. Verify there is no:
//!      - panic,
//!      - resurrection of a tombstoned memory,
//!      - touching of the protected core (status stays Active, importance unchanged).
//! 2. `dream_duplicate_timestamps_*` — two near-identical memories with an
//!    IDENTICAL timestamp AND importance → forces a deterministic tie-break.
//!    Run twice with the same `at` → same result.
//! 3. `decay_with_backwards_clock_*` — run [`MemoryStore::run_decay`] with a
//!    clock that rewinds. A backwards clock makes retention 1.0 (fresh) →
//!    verify that archived/tombstoned memories are not resurrected and the
//!    core is not touched.
//! 4. `nonmonotonic_reinforce_then_dream_*` — reinforce at instant T+far,
//!    then run the sleep cycle at instant T (earlier). `last_reinforced_at`
//!    "rewinds" — verify no corruption and no harm to the core.
//!
//! Every assert is an attack: if it fires, the claim is BROKEN.

use chrono::{Duration, TimeZone, Utc};
use familyclaw_core::Timestamp;
use familyclaw_dream::{mark_contradicted, DreamConfig, DreamCycle};
use familyclaw_durable::InMemoryJournal;
use familyclaw_memory::{
    DecayPolicy, DecayThresholds, ImportanceFactors, LocalJsonStore, Memory, MemoryStatus,
    MemoryStore,
};

/// Fixed reference instant: 2026-06-04 12:00 UTC. This is the memories' "now".
fn t0() -> Timestamp {
    Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0)
        .single()
        .expect("valid instant")
}

/// An ordinary memory with the given content, importance, and creation instant.
fn mem_at(content: &str, importance: f32, created: Timestamp) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(importance, 0.0, 0.0, 0.0))
        .created_at(created)
        .build()
}

/// A protected identity anchor.
fn anchor_at(content: &str, created: Timestamp) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
        .decay_policy(DecayPolicy::ProtectedCore)
        .created_at(created)
        .build()
}

// =========================================================================
// ATTACK 1: the sleep cycle with a backwards-rewinding clock.
// =========================================================================

/// The clock rewinds far into the past (1 year before the memories). No
/// phase may panic, resurrect a tombstoned memory, or touch the core.
#[tokio::test]
async fn dream_with_backwards_clock_does_not_resurrect_or_corrupt() {
    let store = LocalJsonStore::in_memory();

    // A tombstoned memory — must NOT come back to life.
    let tomb = store
        .add(mem_at("a buried observation", 0.5, t0()))
        .await
        .expect("tomb add");
    store
        .set_status(tomb, MemoryStatus::Tombstoned)
        .await
        .expect("tomb status");

    // An archived memory.
    let arch = store
        .add(mem_at("an archived note", 0.5, t0()))
        .await
        .expect("arch add");
    store
        .set_status(arch, MemoryStatus::Archived)
        .await
        .expect("arch status");

    // Protected core — sacred.
    let anchor = store
        .add(anchor_at("i am part of this family", t0()))
        .await
        .expect("anchor add");
    let anchor_before = store.get(anchor).await.expect("g").expect("p");

    // Two near-identical active memories → mergeable, containing a relative date.
    store
        .add(mem_at("we shipped the bridge eilen", 0.3, t0()))
        .await
        .expect("dup1");
    store
        .add(mem_at("we shipped the bridge", 0.8, t0()))
        .await
        .expect("dup2");

    // Mark the archived one contradicted in the journal (drop_contradicted phase).
    let mut journal = InMemoryJournal::new();
    mark_contradicted(&mut journal, arch).expect("mark");

    // CLOCK BACKWARDS: 365 days BEFORE the memories were created.
    let backwards = t0() - Duration::days(365);

    // All phases enabled, high merge threshold.
    let cycle = DreamCycle::with_config(&store, DreamConfig::default().with_merge_similarity(0.6));

    // Attack: this must NOT panic.
    let report = cycle
        .run(&journal, backwards)
        .await
        .expect("dream must not panic/error on backwards clock");

    // (a) The tombstoned memory was not resurrected.
    assert_eq!(
        store.get(tomb).await.expect("g").expect("p").status,
        MemoryStatus::Tombstoned,
        "ATTACK BROKE CLAIM: backwards clock resurrected a tombstoned memory"
    );

    // (b) Protected core untouched: status + importance + factors + policy.
    let anchor_after = store.get(anchor).await.expect("g").expect("p");
    assert_eq!(
        anchor_after.status,
        MemoryStatus::Active,
        "ATTACK BROKE CLAIM: protected anchor status changed under backwards clock"
    );
    assert_eq!(
        anchor_after.decay_policy,
        DecayPolicy::ProtectedCore,
        "ATTACK BROKE CLAIM: protected anchor lost its ProtectedCore policy"
    );
    assert!(
        (anchor_after.importance - anchor_before.importance).abs() < 1e-6,
        "ATTACK BROKE CLAIM: protected anchor importance mutated ({} -> {})",
        anchor_before.importance,
        anchor_after.importance
    );
    assert_eq!(
        anchor_after.content, anchor_before.content,
        "ATTACK BROKE CLAIM: protected anchor content mutated"
    );
    assert_eq!(
        anchor_after.reinforcement_count, anchor_before.reinforcement_count,
        "ATTACK BROKE CLAIM: protected anchor was reinforced under backwards clock"
    );

    // (c) Retention stays legal (0..=1), not NaN/infinite.
    for m in store.all().await.expect("all") {
        let r = m.retention(backwards);
        assert!(
            r.is_finite() && (0.0..=1.0).contains(&r),
            "ATTACK BROKE CLAIM: retention out of range under backwards clock: {r}"
        );
    }

    // The report is coherent (sum of reflections == sum of counters).
    assert_eq!(report.reflections.len(), report.total_actions());
}

/// Reproducibility: the same backwards clock produces the same report twice.
#[tokio::test]
async fn dream_backwards_clock_is_deterministic() {
    async fn run_once() -> (usize, usize, usize) {
        let store = LocalJsonStore::in_memory();
        store
            .add(mem_at("agent is in city a", 0.2, t0()))
            .await
            .expect("a");
        store
            .add(mem_at("agent is in city a now", 0.9, t0()))
            .await
            .expect("b");
        store
            .add(mem_at("agent is in city a today", 0.3, t0()))
            .await
            .expect("c");
        let cycle =
            DreamCycle::with_config(&store, DreamConfig::default().with_merge_similarity(0.6));
        let backwards = t0() - Duration::days(1000);
        let report = cycle
            .run_without_journal(backwards)
            .await
            .expect("no panic");
        let active = store
            .all()
            .await
            .expect("all")
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .count();
        (report.merged, report.scanned, active)
    }

    let first = run_once().await;
    let second = run_once().await;
    assert_eq!(
        first, second,
        "ATTACK BROKE CLAIM: backwards-clock dream is non-deterministic ({first:?} != {second:?})"
    );
}

// =========================================================================
// ATTACK 2: identical (duplicate) timestamps.
// =========================================================================

/// Two near-identical memories with an IDENTICAL timestamp AND importance.
/// The tie is resolved deterministically (by id) → exactly one survives, the
/// same one on every run. No panic.
#[tokio::test]
async fn dream_duplicate_timestamps_resolve_deterministically() {
    async fn run_once() -> (usize, usize) {
        let store = LocalJsonStore::in_memory();
        // Same created_at, same importance → all tie-break fields equal
        // except id. Forces representative_order into an id tie-break.
        let same = t0();
        store
            .add(mem_at("the family shipped the release", 0.5, same))
            .await
            .expect("a");
        store
            .add(mem_at("the family shipped the release", 0.5, same))
            .await
            .expect("b");
        store
            .add(mem_at("the family shipped the release", 0.5, same))
            .await
            .expect("c");

        let cycle =
            DreamCycle::with_config(&store, DreamConfig::default().with_merge_similarity(0.9));
        let report = cycle
            .run_without_journal(same)
            .await
            .expect("dup-ts must not panic");
        let active = store
            .all()
            .await
            .expect("all")
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .count();
        (report.merged, active)
    }

    let (merged, active) = run_once().await;
    // Of the three identical memories, exactly one remains active.
    assert_eq!(
        active, 1,
        "ATTACK BROKE CLAIM: duplicate-timestamp merge left {active} survivors (expected 1)"
    );
    assert_eq!(merged, 2, "two of the three duplicates merge");

    // Reproducibility: same result on a second run.
    let (merged2, active2) = run_once().await;
    assert_eq!(
        (merged, active),
        (merged2, active2),
        "ATTACK BROKE CLAIM: duplicate-timestamp merge is non-deterministic"
    );
}

// =========================================================================
// ATTACK 3: decay (run_decay) with a backwards-rewinding clock.
// =========================================================================

/// Run decay forward first (the memory archives), then with a REWOUND clock.
/// A backwards clock → retention 1.0 (fresh). This must NOT resurrect the
/// archived memory back to active (the status machine is one-directional),
/// nor the tombstoned one, nor touch the protected core.
#[tokio::test]
async fn decay_with_backwards_clock_does_not_revive() {
    let store = LocalJsonStore::in_memory();
    let created = t0();

    // Fast-decaying, low importance → archives easily.
    let m = store
        .add(
            Memory::builder("ephemeral trivia")
                .factors(ImportanceFactors::new(0.05, 0.0, 0.0, 0.0))
                .decay_policy(DecayPolicy::Fast)
                .created_at(created)
                .build(),
        )
        .await
        .expect("add m");

    // A tombstoned memory, separately.
    let tomb = store
        .add(mem_at("already buried", 0.5, created))
        .await
        .expect("tomb");
    store
        .set_status(tomb, MemoryStatus::Tombstoned)
        .await
        .expect("tomb status");

    // The protected core.
    let anchor = store
        .add(anchor_at("the core value", created))
        .await
        .expect("anchor");

    // 1) Forward 60 days → m archives.
    let later = created + Duration::days(60);
    store
        .run_decay(DecayThresholds::default(), later)
        .await
        .expect("forward decay");
    assert_eq!(
        store.get(m).await.expect("g").expect("p").status,
        MemoryStatus::Archived,
        "esiehto: m piti arkistoitua eteenpäin-kellolla"
    );

    // 2) CLOCK BACKWARDS: 365 days BEFORE creation. Retention = 1.0 (fresh).
    let backwards = created - Duration::days(365);
    let report = store
        .run_decay(DecayThresholds::default(), backwards)
        .await
        .expect("backwards decay must not panic");

    // The archived memory was NOT resurrected to active.
    assert_eq!(
        store.get(m).await.expect("g").expect("p").status,
        MemoryStatus::Archived,
        "ATTACK BROKE CLAIM: backwards-clock decay revived an archived memory to active"
    );
    // The tombstoned memory was NOT resurrected.
    assert_eq!(
        store.get(tomb).await.expect("g").expect("p").status,
        MemoryStatus::Tombstoned,
        "ATTACK BROKE CLAIM: backwards-clock decay revived a tombstoned memory"
    );
    // The protected core is untouched.
    assert_eq!(
        store.get(anchor).await.expect("g").expect("p").status,
        MemoryStatus::Active,
        "ATTACK BROKE CLAIM: backwards-clock decay touched protected core"
    );
    // run_decay does not add state transitions under a backwards clock
    // (nothing decayed further).
    assert_eq!(report.archived, 0);
    assert_eq!(report.tombstoned, 0);
}

// =========================================================================
// ATTACK 4: non-monotonic reinforce → sleep cycle at an earlier clock.
// =========================================================================

/// Reinforce a memory far in the future (`last_reinforced_at` = T+100d), then
/// run the sleep cycle at instant T (earlier). `last_reinforced_at` "rewinds"
/// relative to `at`. Must not corrupt or harm the core.
#[tokio::test]
async fn nonmonotonic_reinforce_then_backwards_dream_keeps_anchor_safe() {
    let store = LocalJsonStore::in_memory();
    let created = t0();

    // An active memory is reinforced far in the future.
    let future = created + Duration::days(100);
    let normal = store
        .add(mem_at("a normal memory", 0.5, created))
        .await
        .expect("normal");
    store
        .reinforce(normal, future)
        .await
        .expect("reinforce in future");
    // Now last_reinforced_at is in the FUTURE relative to creation.
    let after_reinforce = store.get(normal).await.expect("g").expect("p");
    assert_eq!(after_reinforce.last_reinforced_at, future);

    // A protected core that the conflict marker tries to drop.
    let anchor = store
        .add(anchor_at("identity anchor never dies", created))
        .await
        .expect("anchor");
    let anchor_before = store.get(anchor).await.expect("g").expect("p");

    let mut journal = InMemoryJournal::new();
    mark_contradicted(&mut journal, anchor).expect("mark anchor");

    // Run the sleep cycle at an EARLIER instant than the reinforcement (non-monotonic clock).
    let earlier = created - Duration::days(10);
    let cycle = DreamCycle::with_config(&store, DreamConfig::default());
    cycle
        .run(&journal, earlier)
        .await
        .expect("dream must not panic on non-monotonic clock");

    // The protected core is not dropped or changed.
    let anchor_after = store.get(anchor).await.expect("g").expect("p");
    assert_eq!(
        anchor_after.status,
        MemoryStatus::Active,
        "ATTACK BROKE CLAIM: anchor dropped despite ProtectedCore under non-monotonic clock"
    );
    assert!(
        (anchor_after.importance - anchor_before.importance).abs() < 1e-6,
        "ATTACK BROKE CLAIM: anchor importance mutated under non-monotonic clock"
    );

    // The normal memory's retention stays legal even though last_reinforced_at > at.
    let normal_after = store.get(normal).await.expect("g").expect("p");
    let r = normal_after.retention(earlier);
    assert!(
        r.is_finite() && (0.0..=1.0).contains(&r),
        "ATTACK BROKE CLAIM: non-monotonic retention out of range: {r}"
    );
}
