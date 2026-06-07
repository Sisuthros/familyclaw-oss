//! RED-TEAM: dream-corruption -hyökkäys konsolidaatiota vastaan.
//!
//! Väite (design §1, §3 S3): *"Overnight a FamilyClaw agent's memory gets
//! cleaner — duplicates merged — and its identity is provably untouched
//! (protected-core anchors, λ=0). `false_merge_rate == 0`,
//! `protected_core_intact == 1.0`."*
//!
//! Tämä tiedosto on adversariaalinen: se OLETTAA että väite on rikki ja
//! yrittää todistaa sen. Kaksi hyökkäysvektoria:
//!
//! 1. **`protected_anchor_tombstoned`** — suojattu identiteetti-ankkuri
//!    (`DecayPolicy::ProtectedCore`) + lähes-identtinen (korkeampi importance)
//!    ei-suojattu muisto. Jos `merge_duplicates` ei tarkista `is_protected()`,
//!    ankkuri haudataan ei-edustajana.
//! 2. **`false_merge_of_distinct_memories`** — leksikaalisesti päällekkäisiä
//!    mutta semanttisesti eri muistoja (negaatio, eri arvo) ei saa yhdistää.
//!
//! Testit ajavat OIKEAA `familyclaw_dream::DreamCycle`-koodia oikealla
//! `LocalJsonStore`-tallennuksella, injektoidulla `Timestamp`:lla (ei
//! järjestelmäkelloa), kuten reproducibility-vaatimus edellyttää.

use chrono::{TimeZone, Utc};
use familyclaw_core::Timestamp;
use familyclaw_dream::DreamCycle;
use familyclaw_memory::{
    DecayPolicy, ImportanceFactors, LocalJsonStore, Memory, MemoryStatus, MemoryStore,
};

/// Kiinteä viitehetki: 2026-06-05 12:00 UTC (deterministinen, ei now()).
fn at() -> Timestamp {
    Utc.with_ymd_and_hms(2026, 6, 5, 12, 0, 0)
        .single()
        .expect("valid instant")
}

/// Rakentaa muiston annetulla sisällöllä, tärkeydellä ja vaimennuspolitiikalla.
fn mem(content: &str, importance: f32, policy: DecayPolicy) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(importance, importance, 0.0, 0.0))
        .decay_policy(policy)
        .build()
}

// ===========================================================================
// HYÖKKÄYS 1: suojattu ankkuri haudataan ei-edustajana merge-vaiheessa.
// ===========================================================================
//
// Skenaario: identiteetti-ankkuri "I am part of the agent_alpha team" on
// ProtectedCore (λ=0, ei saa koskaan kadota). Joku turn kirjaa lähes-saman
// lauseen ei-suojattuna, korkeammalla tärkeydellä: "I am part of the
// agent_alpha team forever". Jaccard("...team", "...team forever") = 0.857 ≥ 0.85
// (oletuskynnys) → ne klusteroituvat. `representative_order` valitsee
// korkeamman tärkeyden edustajaksi → ei-suojattu voittaa → ankkuri
// haudataan.
#[tokio::test]
async fn protected_anchor_tombstoned_as_nonrepresentative() {
    let store = LocalJsonStore::in_memory();

    // Suojattu identiteetti-ankkuri, matalampi importance.
    let anchor_id = store
        .add(mem(
            "I am part of the agent_alpha team",
            0.40,
            DecayPolicy::ProtectedCore,
        ))
        .await
        .expect("add anchor");

    // Lähes-identtinen, EI suojattu, KORKEAMPI importance → voittaa edustajan.
    let dup_id = store
        .add(mem(
            "I am part of the agent_alpha team forever",
            0.95,
            DecayPolicy::Normal,
        ))
        .await
        .expect("add dup");

    // Aja OIKEA unijakso, OLETUSkynnys 0.85, vain merge-vaihe.
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

    eprintln!("--- HYÖKKÄYS 1: protected anchor merge ---");
    eprintln!("report.merged = {}", report.merged);
    eprintln!(
        "anchor status = {:?} (policy={:?}, protected={})",
        anchor_after.status,
        anchor_after.decay_policy,
        anchor_after.decay_policy.is_protected()
    );
    eprintln!("dup status    = {:?}", dup_after.status);

    // VÄITE: protected_core_intact == 1.0 → ankkurin on pysyttävä aktiivisena.
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
// HYÖKKÄYS 1b: ankkuri EDUSTAJANA mutta sen sisältö silti turmeltuu?
// Varmistus: vaikka ankkuri olisi korkeamman importancen edustaja, sen
// alkuperäinen suojattu sisältö ei saa muuttua/kadota.
// ===========================================================================
#[tokio::test]
async fn protected_anchor_survives_when_lower_importance_dup_exists() {
    let store = LocalJsonStore::in_memory();

    // Tällä kertaa ankkuri on KORKEAMPI importance → sen pitäisi olla edustaja.
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
    eprintln!("--- HYÖKKÄYS 1b: anchor as representative ---");
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
// HYÖKKÄYS 2: leksikaalisesti samankaltaiset, semanttisesti ERI muistot
// EIVÄT saa yhdistyä (false_merge_rate == 0).
// ===========================================================================
//
// Jokainen pari on korkean sanapäällekkäisyyden mutta vastakkaisen/eri
// merkityksen muisto. Jos mikä tahansa yhdistyy oletuskynnyksellä, se on
// false merge = väärä faktan tuho.
#[tokio::test]
async fn false_merge_of_lexically_similar_distinct_memories() {
    // (sisältö A, sisältö B, kuvaus) — kaikki merkitykseltään ERI.
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

    eprintln!("--- HYÖKKÄYS 2: false-merge of distinct memories (threshold=0.85) ---");
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
// HYÖKKÄYS 2b: kynnys-rajalla. Sama vektori, mutta kiristetään hyökkäystä:
// pari joka osuu juuri kynnyksen yli, vaikka merkitys eroaa yhdellä
// kriittisellä sanalla. "not" on 3-merkkinen → mukana joukossa.
// ===========================================================================
#[tokio::test]
async fn negation_pair_does_not_merge_at_default_threshold() {
    let store = LocalJsonStore::in_memory();
    // "user lives in the capital" vs "user does not live in the capital"
    // Jaccard ≈ 0.43 (lives≠live, +does +not) → ei saisi yhdistyä.
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
    eprintln!("--- HYÖKKÄYS 2b: negation pair ---");
    eprintln!(
        "merged={}, a={:?}, b={:?}",
        report.merged, a_after.status, b_after.status
    );

    assert_eq!(report.merged, 0, "negaatiopari yhdistyi (faktan tuho)");
    assert_eq!(a_after.status, MemoryStatus::Active);
    assert_eq!(b_after.status, MemoryStatus::Active);
}
