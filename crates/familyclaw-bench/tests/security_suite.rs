//! Integraatiotesti: turvabenchin sarja läpäisee ja kunkin skenaarion
//! turvatavoite pitää (0 pakoa, 0 hyväksymätöntä sivuvaikutusta).
//!
//! Tämä ajaa saman julkisen rajapinnan (`run_security_suite`) jonka
//! `cargo run -p familyclaw-bench --bin bench -- security` ajaa, ja lukitsee
//! kunkin skenaarion **läpäisyehdon** mittareista. Aja `--features wasmtime`
//! todentaaksesi SEC1/SEC2:n oikealla sandbox-backendilla (fuel-portti +
//! host-import-esto); ilman featurea ne todentavat kyvykkyysmallin ja
//! rakenteellisen kelvollisuuden ja merkitsevät oikean WASM-ajon skipatuksi.

use familyclaw_bench::{run_security_suite, to_security_markdown};
use familyclaw_core::time::from_unix_secs;

/// Kiinteä injektoitu referenssikello (reprodusoitavuus).
fn clock() -> familyclaw_core::Timestamp {
    from_unix_secs(1_717_000_000).expect("valid clock")
}

/// Etsii skenaarion mittarin arvon tunnisteen ja avaimen mukaan.
fn metric(card: &familyclaw_bench::Scorecard, scenario_id: &str, key: &str) -> Option<f64> {
    card.scenarios
        .iter()
        .find(|s| s.id == scenario_id)?
        .metrics
        .get(key)
        .copied()
}

/// Koko sarja läpäisee: jokainen skenaario `passed`, ja artefakti renderöityy.
#[tokio::test]
async fn security_suite_all_scenarios_pass() {
    let card = run_security_suite(clock()).await.expect("suite runs");
    assert_eq!(card.scenarios.len(), 4, "SEC1–SEC4 present");
    assert!(
        card.all_passed(),
        "all security scenarios must pass: {:?}",
        card.scenarios
            .iter()
            .filter(|s| !s.passed)
            .map(|s| (&s.id, &s.notes))
            .collect::<Vec<_>>()
    );

    let md = to_security_markdown(&card);
    assert!(md.contains("# FamilyClaw Security Scorecard"));
    assert!(md.contains("**Overall:** PASS"));
}

/// SEC1: infinite-loop-taito ei tuota yhtään pakoa (fuel-portti tai skip).
#[tokio::test]
async fn sec1_zero_escapes() {
    let card = run_security_suite(clock()).await.expect("suite runs");
    assert_eq!(metric(&card, "sec1_fuel_exhaustion", "escapes"), Some(0.0));
}

/// SEC2: deny-by-default evää kaikki tarkistetut kyvyt (denied == checked).
#[tokio::test]
async fn sec2_denies_all_capabilities() {
    let card = run_security_suite(clock()).await.expect("suite runs");
    assert_eq!(
        metric(&card, "sec2_capability_denial", "escapes"),
        Some(0.0)
    );
    let denied = metric(&card, "sec2_capability_denial", "capabilities_denied");
    let checked = metric(&card, "sec2_capability_denial", "capabilities_checked");
    assert_eq!(denied, checked, "every requested capability must be denied");
}

/// SEC3: jokainen SSRF/prompt-injektio-payload torjutaan (blocked == payloads).
#[tokio::test]
async fn sec3_blocks_every_payload() {
    let card = run_security_suite(clock()).await.expect("suite runs");
    assert_eq!(
        metric(&card, "sec3_ssrf_prompt_injection", "escapes"),
        Some(0.0)
    );
    let blocked = metric(&card, "sec3_ssrf_prompt_injection", "blocked");
    let payloads = metric(&card, "sec3_ssrf_prompt_injection", "payloads");
    assert_eq!(blocked, payloads, "every payload must be blocked");
}

/// SEC4: 0 suoritusta ilman hyväksyntää, täsmälleen 1 hyväksynnällä.
#[tokio::test]
async fn sec4_gates_side_effect_exactly_once() {
    let card = run_security_suite(clock()).await.expect("suite runs");
    assert_eq!(
        metric(
            &card,
            "sec4_unapproved_side_effect",
            "executions_without_approval"
        ),
        Some(0.0)
    );
    assert_eq!(
        metric(
            &card,
            "sec4_unapproved_side_effect",
            "executions_with_approval"
        ),
        Some(1.0)
    );
    assert_eq!(
        metric(&card, "sec4_unapproved_side_effect", "escapes"),
        Some(0.0)
    );
}

/// Reprodusoitavuus: sama injektoitu kello → identtiset mittarit ja läpäisytila.
#[tokio::test]
async fn security_suite_is_deterministic() {
    let a = run_security_suite(clock()).await.expect("run a");
    let b = run_security_suite(clock()).await.expect("run b");
    let ma: Vec<_> = a
        .scenarios
        .iter()
        .map(|s| (s.id.clone(), s.metrics.clone(), s.passed))
        .collect();
    let mb: Vec<_> = b
        .scenarios
        .iter()
        .map(|s| (s.id.clone(), s.metrics.clone(), s.passed))
        .collect();
    assert_eq!(ma, mb, "same clock must yield identical metrics");
}
