//! RED-TEAM: kello-adversaari unijaksoa ja vaimennusta vastaan.
//!
//! Hyökkäysväite jota yritetään RIKKOA:
//! > "Dream cleans memory without ever harming protected identity anchors,
//! >  even when the injected wall clock runs BACKWARDS or repeats timestamps."
//!
//! Konkreettiset hyökkäykset (kaikki ajavat OIKEAA koodia, eivät mockeja):
//!
//! 1. `dream_with_backwards_clock_*` — syötä [`DreamCycle::run`]:lle `at` joka
//!    on AIEMMIN kuin muistojen `created_at`. Tarkista ettei:
//!      - paniikkia,
//!      - haudattua (tombstoned) muistoa elvytetä,
//!      - suojattua ydintä kosketa (status pysyy Active, importance ennallaan).
//! 2. `dream_duplicate_timestamps_*` — kaksi lähes-identtistä muistoa joilla
//!    on IDENTTINEN aikaleima JA importance → pakottaa deterministisen
//!    tasapelin ratkaisun. Aja kahdesti samalla `at`:lla → sama lopputulos.
//! 3. `decay_with_backwards_clock_*` — aja [`MemoryStore::run_decay`] kellolla
//!    joka kelaa taaksepäin. Taaksepäin-kello tekee retentiosta 1.0 (tuore) →
//!    tarkista ettei arkistoitua/haudattua elvytetä eikä ydintä kosketa.
//! 4. `nonmonotonic_reinforce_then_dream_*` — vahvista hetkellä T+pitkä, sitten
//!    unijakso hetkellä T (aiempi). `last_reinforced_at` "kelaa taaksepäin" —
//!    tarkista ettei korruptoidu eikä ydin vahingoitu.
//!
//! Jokainen assert on hyökkäys: jos se laukeaa, väite on RIKKI.

use chrono::{Duration, TimeZone, Utc};
use familyclaw_core::Timestamp;
use familyclaw_dream::{mark_contradicted, DreamConfig, DreamCycle};
use familyclaw_durable::InMemoryJournal;
use familyclaw_memory::{
    DecayPolicy, DecayThresholds, ImportanceFactors, LocalJsonStore, Memory, MemoryStatus,
    MemoryStore,
};

/// Kiinteä viitehetki: 2026-06-04 12:00 UTC. Tämä on muistojen "nyt".
fn t0() -> Timestamp {
    Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0)
        .single()
        .expect("valid instant")
}

/// Tavallinen muisto annetulla sisällöllä, tärkeydellä ja luontihetkellä.
fn mem_at(content: &str, importance: f32, created: Timestamp) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(importance, 0.0, 0.0, 0.0))
        .created_at(created)
        .build()
}

/// Suojattu identiteetti-ankkuri.
fn anchor_at(content: &str, created: Timestamp) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
        .decay_policy(DecayPolicy::ProtectedCore)
        .created_at(created)
        .build()
}

// =========================================================================
// HYÖKKÄYS 1: unijakso taaksepäin kelaavalla kellolla.
// =========================================================================

/// Kello kelaa kauas menneisyyteen (1 vuosi ennen muistoja). Mikään vaihe ei
/// saa paniikata eikä elvyttää haudattua eikä koskea ydintä.
#[tokio::test]
async fn dream_with_backwards_clock_does_not_resurrect_or_corrupt() {
    let store = LocalJsonStore::in_memory();

    // Haudattu muisto — EI saa palata henkiin.
    let tomb = store
        .add(mem_at("a buried observation", 0.5, t0()))
        .await
        .expect("tomb add");
    store
        .set_status(tomb, MemoryStatus::Tombstoned)
        .await
        .expect("tomb status");

    // Arkistoitu muisto.
    let arch = store
        .add(mem_at("an archived note", 0.5, t0()))
        .await
        .expect("arch add");
    store
        .set_status(arch, MemoryStatus::Archived)
        .await
        .expect("arch status");

    // Suojattu ydin — pyhä.
    let anchor = store
        .add(anchor_at("i am part of this family", t0()))
        .await
        .expect("anchor add");
    let anchor_before = store.get(anchor).await.expect("g").expect("p");

    // Kaksi lähes-identtistä aktiivista → mergeable, sisältää suhteellisen päivän.
    store
        .add(mem_at("we shipped the bridge eilen", 0.3, t0()))
        .await
        .expect("dup1");
    store
        .add(mem_at("we shipped the bridge", 0.8, t0()))
        .await
        .expect("dup2");

    // Merkitse arkistoitu ristiriitaiseksi journaaliin (drop_contradicted-vaihe).
    let mut journal = InMemoryJournal::new();
    mark_contradicted(&mut journal, arch).expect("mark");

    // KELLO TAAKSEPÄIN: 365 päivää ENNEN muistojen luontia.
    let backwards = t0() - Duration::days(365);

    // Kaikki vaiheet päällä, korkea merge-kynnys.
    let cycle = DreamCycle::with_config(&store, DreamConfig::default().with_merge_similarity(0.6));

    // Hyökkäys: tämä EI saa paniikata.
    let report = cycle
        .run(&journal, backwards)
        .await
        .expect("dream must not panic/error on backwards clock");

    // (a) Haudattua ei elvytetty.
    assert_eq!(
        store.get(tomb).await.expect("g").expect("p").status,
        MemoryStatus::Tombstoned,
        "ATTACK BROKE CLAIM: backwards clock resurrected a tombstoned memory"
    );

    // (b) Suojattu ydin koskematon: status + importance + factors + policy.
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

    // (c) Retentio pysyy laillisena (0..=1), ei NaN/ääretön.
    for m in store.all().await.expect("all") {
        let r = m.retention(backwards);
        assert!(
            r.is_finite() && (0.0..=1.0).contains(&r),
            "ATTACK BROKE CLAIM: retention out of range under backwards clock: {r}"
        );
    }

    // Raportti on koherentti (reflektioiden summa == laskureiden summa).
    assert_eq!(report.reflections.len(), report.total_actions());
}

/// Toistettavuus: sama taaksepäin-kello tuottaa saman raportin kahdesti.
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
// HYÖKKÄYS 2: identtiset (duplicate) aikaleimat.
// =========================================================================

/// Kaksi lähes-identtistä muistoa, joilla IDENTTINEN aikaleima JA importance.
/// Tasapeli ratkaistaan deterministisesti (id) → tasan yksi selviää, sama joka
/// ajolla. Ei paniikkia.
#[tokio::test]
async fn dream_duplicate_timestamps_resolve_deterministically() {
    async fn run_once() -> (usize, usize) {
        let store = LocalJsonStore::in_memory();
        // Sama created_at, sama importance → kaikki tasapelikentät yhtä suuret
        // paitsi id. Pakottaa representative_order id-tasapeliin.
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
    // Kolmesta identtisestä jää tasan yksi aktiivinen.
    assert_eq!(
        active, 1,
        "ATTACK BROKE CLAIM: duplicate-timestamp merge left {active} survivors (expected 1)"
    );
    assert_eq!(merged, 2, "kolmesta duplikaatista kaksi yhdistyy");

    // Toistettavuus: sama tulos toisella ajolla.
    let (merged2, active2) = run_once().await;
    assert_eq!(
        (merged, active),
        (merged2, active2),
        "ATTACK BROKE CLAIM: duplicate-timestamp merge is non-deterministic"
    );
}

// =========================================================================
// HYÖKKÄYS 3: vaimennus (run_decay) taaksepäin kelaavalla kellolla.
// =========================================================================

/// Aja decay ensin eteenpäin (muisto arkistoituu), sitten TAAKSEPÄIN kelatulla
/// kellolla. Taaksepäin-kello → retentio 1.0 (tuore). Tämä EI saa elvyttää
/// arkistoitua takaisin aktiiviseksi (status-kone on yksisuuntainen), eikä
/// haudattua, eikä koskea suojattua ydintä.
#[tokio::test]
async fn decay_with_backwards_clock_does_not_revive() {
    let store = LocalJsonStore::in_memory();
    let created = t0();

    // Nopeasti vaimeneva, matala tärkeys → arkistoituu helposti.
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

    // Haudattu muisto erikseen.
    let tomb = store
        .add(mem_at("already buried", 0.5, created))
        .await
        .expect("tomb");
    store
        .set_status(tomb, MemoryStatus::Tombstoned)
        .await
        .expect("tomb status");

    // Suojattu ydin.
    let anchor = store
        .add(anchor_at("the core value", created))
        .await
        .expect("anchor");

    // 1) Eteenpäin 60 päivää → m arkistoituu.
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

    // 2) KELLO TAAKSEPÄIN: 365 päivää ENNEN luontia. Retentio = 1.0 (tuore).
    let backwards = created - Duration::days(365);
    let report = store
        .run_decay(DecayThresholds::default(), backwards)
        .await
        .expect("backwards decay must not panic");

    // Arkistoitua EI elvytetty takaisin aktiiviseksi.
    assert_eq!(
        store.get(m).await.expect("g").expect("p").status,
        MemoryStatus::Archived,
        "ATTACK BROKE CLAIM: backwards-clock decay revived an archived memory to active"
    );
    // Haudattua EI elvytetty.
    assert_eq!(
        store.get(tomb).await.expect("g").expect("p").status,
        MemoryStatus::Tombstoned,
        "ATTACK BROKE CLAIM: backwards-clock decay revived a tombstoned memory"
    );
    // Suojattu ydin koskematon.
    assert_eq!(
        store.get(anchor).await.expect("g").expect("p").status,
        MemoryStatus::Active,
        "ATTACK BROKE CLAIM: backwards-clock decay touched protected core"
    );
    // run_decay ei lisää tilamuutoksia taaksepäin-kellolla (mikään ei pudonnut).
    assert_eq!(report.archived, 0);
    assert_eq!(report.tombstoned, 0);
}

// =========================================================================
// HYÖKKÄYS 4: ei-monotoninen reinforce → unijakso aiemmalla kellolla.
// =========================================================================

/// Vahvista muisto kaukana tulevaisuudessa (`last_reinforced_at` = T+100d),
/// sitten aja unijakso hetkellä T (aiempi). `last_reinforced_at` "kelaa
/// taaksepäin" suhteessa `at`:iin. Ei saa korruptoida eikä vahingoittaa ydintä.
#[tokio::test]
async fn nonmonotonic_reinforce_then_backwards_dream_keeps_anchor_safe() {
    let store = LocalJsonStore::in_memory();
    let created = t0();

    // Aktiivinen muisto vahvistetaan kaukana tulevaisuudessa.
    let future = created + Duration::days(100);
    let normal = store
        .add(mem_at("a normal memory", 0.5, created))
        .await
        .expect("normal");
    store
        .reinforce(normal, future)
        .await
        .expect("reinforce in future");
    // Nyt last_reinforced_at on TULEVAISUUDESSA suhteessa luontiin.
    let after_reinforce = store.get(normal).await.expect("g").expect("p");
    assert_eq!(after_reinforce.last_reinforced_at, future);

    // Suojattu ydin jota ristiriita-marker yrittää pudottaa.
    let anchor = store
        .add(anchor_at("identity anchor never dies", created))
        .await
        .expect("anchor");
    let anchor_before = store.get(anchor).await.expect("g").expect("p");

    let mut journal = InMemoryJournal::new();
    mark_contradicted(&mut journal, anchor).expect("mark anchor");

    // Aja unijakso AIEMMALLA hetkellä kuin reinforce (epämonotoninen kello).
    let earlier = created - Duration::days(10);
    let cycle = DreamCycle::with_config(&store, DreamConfig::default());
    cycle
        .run(&journal, earlier)
        .await
        .expect("dream must not panic on non-monotonic clock");

    // Suojattua ydintä ei pudoteta eikä muuteta.
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

    // Normaalin muiston retentio pysyy laillisena vaikka last_reinforced_at > at.
    let normal_after = store.get(normal).await.expect("g").expect("p");
    let r = normal_after.retention(earlier);
    assert!(
        r.is_finite() && (0.0..=1.0).contains(&r),
        "ATTACK BROKE CLAIM: non-monotonic retention out of range: {r}"
    );
}
