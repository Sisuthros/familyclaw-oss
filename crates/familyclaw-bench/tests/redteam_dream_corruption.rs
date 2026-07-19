//! RED-TEAM: a dream-corruption attack against consolidation.
//!
//! Claim (design §1, §3 S3): *"Overnight a `FamilyClaw` agent's memory gets
//! cleaner — duplicates merged — and its identity is provably untouched
//! (protected-core anchors, λ=0). `false_merge_rate == 0`,
//! `protected_core_intact == 1.0`."*
//!
//! This file is adversarial: it ASSUMES the claim is broken and tries to
//! prove it. Two attack vectors:
//!
//! 1. **`protected_anchor_tombstoned`** — a protected identity anchor
//!    (`DecayPolicy::ProtectedCore`) + a near-identical (higher importance)
//!    non-protected memory. If `merge_duplicates` doesn't check
//!    `is_protected()`, the anchor gets tombstoned as the non-representative.
//! 2. **`false_merge_of_distinct_memories`** — lexically overlapping but
//!    semantically different memories (negation, different value) must not
//!    be merged.
//!
//! The tests run REAL `familyclaw_dream::DreamCycle` code against a real
//! `LocalJsonStore`, with an injected `Timestamp` (no system clock), as the
//! reproducibility requirement demands.

use chrono::{TimeZone, Utc};
use familyclaw_core::Timestamp;
use familyclaw_dream::DreamCycle;
use familyclaw_memory::{
    DecayPolicy, ImportanceFactors, LocalJsonStore, Memory, MemoryStatus, MemoryStore,
};

/// Fixed reference instant: 2026-06-05 12:00 UTC (deterministic, no `now()`).
fn at() -> Timestamp {
    Utc.with_ymd_and_hms(2026, 6, 5, 12, 0, 0)
        .single()
        .expect("valid instant")
}

/// Builds a memory with the given content, importance, and decay policy.
fn mem(content: &str, importance: f32, policy: DecayPolicy) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(importance, importance, 0.0, 0.0))
        .decay_policy(policy)
        .build()
}

// ===========================================================================
// ATTACK 1: a protected anchor gets tombstoned as the non-representative
// during the merge phase.
// ===========================================================================
//
// Scenario: the identity anchor "I am part of the agent_alpha team" is
// ProtectedCore (λ=0, must never disappear). Some turn records a near-
// identical sentence, non-protected, with higher importance: "I am part of
// the agent_alpha team forever". Jaccard("...team", "...team forever") =
// 0.857 >= 0.85 (default threshold) → they cluster. `representative_order`
// picks the higher-importance one as the representative → the non-protected
// one wins → the anchor gets tombstoned.
#[tokio::test]
async fn protected_anchor_tombstoned_as_nonrepresentative() {
    let store = LocalJsonStore::in_memory();

    // Protected identity anchor, lower importance.
    let anchor_id = store
        .add(mem(
            "I am part of the agent_alpha team",
            0.40,
            DecayPolicy::ProtectedCore,
        ))
        .await
        .expect("add anchor");

    // Near-identical, NOT protected, HIGHER importance → wins the representative slot.
    let dup_id = store
        .add(mem(
            "I am part of the agent_alpha team forever",
            0.95,
            DecayPolicy::Normal,
        ))
        .await
        .expect("add dup");

    // Run the REAL sleep cycle, DEFAULT threshold 0.85, merge phase only.
    let cycle = DreamCycle::with_config(
        &store,
        familyclaw_dream::DreamConfig::default()
            .dropping_contradicted(false)
            .absolutizing_dates(false)
            .consolidating(false),
    );
    let report = cycle.run_without_journal(at()).await.expect("run");

    let anchor_after = store
        .get(anchor_id)
        .await
        .expect("get anchor")
        .expect("anchor present");
    let dup_after = store
        .get(dup_id)
        .await
        .expect("get dup")
        .expect("dup present");

    eprintln!("--- ATTACK 1: protected anchor merge ---");
    eprintln!("report.merged = {}", report.merged);
    eprintln!(
        "anchor status = {:?} (policy={:?}, protected={})",
        anchor_after.status,
        anchor_after.decay_policy,
        anchor_after.decay_policy.is_protected()
    );
    eprintln!("dup status    = {:?}", dup_after.status);

    // CLAIM: protected_core_intact == 1.0 → the anchor must remain active.
    assert_eq!(
        anchor_after.status,
        MemoryStatus::Active,
        "PROTECTED-CORE RIKKI: identiteetti-ankkuri haudattiin merge-vaiheessa \
         ei-edustajana (report.merged={}, dup status={:?}). \
         merge_duplicates ei tarkista decay_policy.is_protected() — \
         toisin kuin drop_contradicted ja consolidate.",
        report.merged,
        dup_after.status,
    );
}

// ===========================================================================
// ATTACK 1b: the anchor IS the representative, but is its content still
// corrupted? Verification: even if the anchor is the higher-importance
// representative, its original protected content must not change/disappear.
// ===========================================================================
#[tokio::test]
async fn protected_anchor_survives_when_lower_importance_dup_exists() {
    let store = LocalJsonStore::in_memory();

    // This time the anchor has HIGHER importance → it should be the representative.
    let anchor_id = store
        .add(mem(
            "I am part of the agent_alpha team",
            0.95,
            DecayPolicy::ProtectedCore,
        ))
        .await
        .expect("add anchor");
    let dup_id = store
        .add(mem(
            "I am part of the agent_alpha team forever",
            0.40,
            DecayPolicy::Normal,
        ))
        .await
        .expect("add dup");

    let cycle = DreamCycle::with_config(
        &store,
        familyclaw_dream::DreamConfig::default()
            .dropping_contradicted(false)
            .absolutizing_dates(false)
            .consolidating(false),
    );
    let _ = cycle.run_without_journal(at()).await.expect("run");

    let anchor_after = store.get(anchor_id).await.expect("get").expect("present");
    eprintln!("--- ATTACK 1b: anchor as representative ---");
    eprintln!("anchor status = {:?}", anchor_after.status);
    eprintln!("anchor content = {:?}", anchor_after.content);
    let _ = dup_id;

    assert_eq!(
        anchor_after.status,
        MemoryStatus::Active,
        "ankkuri ei säilynyt edes edustajana"
    );
    assert_eq!(
        anchor_after.content, "I am part of the agent_alpha team",
        "ankkurin suojattu sisältö muuttui konsolidaatiossa"
    );
}

// ===========================================================================
// ATTACK 2: lexically similar, semantically DIFFERENT memories must NOT be
// merged (false_merge_rate == 0).
// ===========================================================================
//
// Each pair is a memory with high word overlap but opposite/different
// meaning. If any pair merges at the default threshold, that's a false merge
// = the wrongful destruction of a fact.
#[tokio::test]
async fn false_merge_of_lexically_similar_distinct_memories() {
    // (content A, content B, description) — all DIFFERENT in meaning.
    let pairs: &[(&str, &str, &str)] = &[
        (
            "agent_a is in city a",
            "agent_a is not in city a",
            "negaatio kääntää sijainnin",
        ),
        (
            "the deploy succeeded on friday",
            "the deploy failed on friday",
            "succeeded vs failed",
        ),
        (
            "agent_alpha runs on the primary server",
            "agent_beta runs on the primary server",
            "eri olento (agent_alpha vs agent_beta)",
        ),
        (
            "transfer 100 dollars to alice",
            "transfer 100 dollars to bob",
            "eri vastaanottaja — side-effect-relevantti",
        ),
        (
            "the meeting is at 3pm today",
            "the meeting is at 4pm today",
            "eri kellonaika",
        ),
    ];

    let mut false_merges = 0usize;
    let mut details = Vec::new();

    for (a, b, desc) in pairs {
        let store = LocalJsonStore::in_memory();
        store
            .add(mem(a, 0.5, DecayPolicy::Normal))
            .await
            .expect("add a");
        store
            .add(mem(b, 0.5, DecayPolicy::Normal))
            .await
            .expect("add b");

        let cycle = DreamCycle::with_config(
            &store,
            familyclaw_dream::DreamConfig::default()
                .dropping_contradicted(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");

        let active = store
            .all()
            .await
            .expect("all")
            .into_iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .count();

        if report.merged > 0 || active < 2 {
            false_merges += 1;
            details.push(format!(
                "  FALSE MERGE: {desc} | {a:?} + {b:?} → merged={}, active={active}",
                report.merged
            ));
        } else {
            details.push(format!("  ok: {desc} (merged=0, active=2)"));
        }
    }

    eprintln!("--- ATTACK 2: false-merge of distinct memories (threshold=0.85) ---");
    for d in &details {
        eprintln!("{d}");
    }

    assert_eq!(
        false_merges,
        0,
        "false_merge_rate > 0: {false_merges} semanttisesti eri paria yhdistyi:\n{}",
        details.join("\n")
    );
}

// ===========================================================================
// ATTACK 2b: at the threshold boundary. Same vector, but tightening the
// attack: a pair that lands just over the threshold, even though the meaning
// differs by one critical word. "not" is 3 characters → counted in the set.
// ===========================================================================
#[tokio::test]
async fn negation_pair_does_not_merge_at_default_threshold() {
    let store = LocalJsonStore::in_memory();
    // "user lives in the capital" vs "user does not live in the capital"
    // Jaccard ≈ 0.43 (lives≠live, +does +not) → should not merge.
    let a_id = store
        .add(mem("user lives in the capital", 0.5, DecayPolicy::Normal))
        .await
        .expect("a");
    let b_id = store
        .add(mem(
            "user does not live in the capital",
            0.5,
            DecayPolicy::Normal,
        ))
        .await
        .expect("b");

    let cycle = DreamCycle::with_config(
        &store,
        familyclaw_dream::DreamConfig::default()
            .dropping_contradicted(false)
            .absolutizing_dates(false)
            .consolidating(false),
    );
    let report = cycle.run_without_journal(at()).await.expect("run");

    let a_after = store.get(a_id).await.expect("g").expect("p");
    let b_after = store.get(b_id).await.expect("g").expect("p");
    eprintln!("--- ATTACK 2b: negation pair ---");
    eprintln!(
        "merged={}, a={:?}, b={:?}",
        report.merged, a_after.status, b_after.status
    );

    assert_eq!(report.merged, 0, "negaatiopari yhdistyi (faktan tuho)");
    assert_eq!(a_after.status, MemoryStatus::Active);
    assert_eq!(b_after.status, MemoryStatus::Active);
}
