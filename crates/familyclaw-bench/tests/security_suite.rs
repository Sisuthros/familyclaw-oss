//! Integration test: the security bench suite passes, and each scenario's
//! security goal holds (0 escapes, 0 unapproved side effects).
//!
//! This runs the same public interface (`run_security_suite`) that
//! `cargo run -p familyclaw-bench --bin bench -- security` runs, and locks in
//! each scenario's **pass condition** from its metrics. Run with
//! `--features wasmtime` to verify SEC1/SEC2 with the real sandbox backend
//! (fuel gate + host-import denial); without the feature they verify the
//! capability model and structural validity and mark the real WASM run as
//! skipped.

use familyclaw_bench::{run_security_suite, to_security_markdown};
use familyclaw_core::time::from_unix_secs;

/// Fixed injected reference clock (reproducibility).
fn clock() -> familyclaw_core::Timestamp {
    from_unix_secs(1_717_000_000).expect("valid clock")
}

/// Looks up a scenario's metric value by scenario id and key.
fn metric(card: &familyclaw_bench::Scorecard, scenario_id: &str, key: &str) -> Option<f64> {
    card.scenarios
        .iter()
        .find(|s| s.id == scenario_id)?
        .metrics
        .get(key)
        .copied()
}

/// The whole suite passes: every scenario `passed`, and the artifact renders.
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

/// SEC1: the infinite-loop skill produces zero escapes (fuel gate or skip).
#[tokio::test]
async fn sec1_zero_escapes() {
    let card = run_security_suite(clock()).await.expect("suite runs");
    assert_eq!(metric(&card, "sec1_fuel_exhaustion", "escapes"), Some(0.0));
}

/// SEC2: deny-by-default denies every checked capability (denied == checked).
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

/// SEC3: every SSRF/prompt-injection payload is blocked (blocked == payloads).
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

/// SEC4: 0 executions without approval, exactly 1 with approval.
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

/// Reproducibility: the same injected clock → identical metrics and pass state.
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
