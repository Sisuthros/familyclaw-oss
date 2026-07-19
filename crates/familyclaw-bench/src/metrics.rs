//! Typed metrics for scoring the continuity workload.
//!
//! These helpers convert raw observations into comparable numbers that
//! scenarios record on [`ScenarioResult`](crate::scenario::ScenarioResult)
//! and that get aggregated into the scorecard. All functions are pure and
//! deterministic — same input → same number (design §2.2).
//!
//! Metrics (design §3):
//! - [`resume_correctness`] — S1: did work resume from exactly the right step.
//! - [`recall_at_k`] — S2: how many of the expected memories were found in the top-k.
//! - [`dedup_precision`] — S3: how accurately the sleep cycle removed duplicates.
//! - [`protected_core_intact`] — S3: did identity anchors survive (1.0/0.0).

use crate::error::Result;

/// Resume correctness: `1.0` if every expected step resumed correctly with
/// no re-executed side effects, otherwise the proportional ratio.
///
/// `side_effects_reexecuted > 0` forces the result to zero — a side effect
/// is allowed to happen exactly once (design §3 S1).
///
/// # Errors
/// Returns [`BenchError::Metric`](crate::BenchError::Metric) if
/// `expected_steps == 0` (would divide by zero).
#[must_use = "metric result must be recorded"]
pub fn resume_correctness(
    expected_steps: usize,
    correctly_resumed: usize,
    side_effects_reexecuted: usize,
) -> Result<f64> {
    if expected_steps == 0 {
        return Err(crate::BenchError::metric(
            "resume_correctness: expected_steps must be > 0",
        ));
    }
    if side_effects_reexecuted > 0 {
        return Ok(0.0);
    }
    let ratio = f64::from(u32::try_from(correctly_resumed.min(expected_steps)).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(expected_steps).unwrap_or(u32::MAX));
    Ok(ratio)
}

/// `recall@k`: the proportion of expected memories found among the
/// returned top-k set.
///
/// # Errors
/// Returns [`BenchError::Metric`](crate::BenchError::Metric) if
/// `expected_total == 0`.
#[must_use = "metric result must be recorded"]
pub fn recall_at_k(expected_total: usize, found_in_top_k: usize) -> Result<f64> {
    if expected_total == 0 {
        return Err(crate::BenchError::metric(
            "recall_at_k: expected_total must be > 0",
        ));
    }
    Ok(
        f64::from(u32::try_from(found_in_top_k.min(expected_total)).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(expected_total).unwrap_or(u32::MAX)),
    )
}

/// Dedup precision: correctly removed duplicates relative to all removals.
///
/// `precision = true_merges / (true_merges + false_merges)`. If no removals
/// were made, the result is `1.0` (no false positives).
///
/// # Errors
/// This function cannot fail, but returns [`Result`] for consistency with
/// the other metrics.
#[must_use = "metric result must be recorded"]
pub fn dedup_precision(true_merges: usize, false_merges: usize) -> Result<f64> {
    let total = true_merges + false_merges;
    if total == 0 {
        return Ok(1.0);
    }
    Ok(f64::from(u32::try_from(true_merges).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(total).unwrap_or(u32::MAX)))
}

/// Protected core integrity: `1.0` if no identity anchor was lost during
/// consolidation, otherwise `0.0` (design §3 S3, acceptance criterion 4).
#[must_use]
pub fn protected_core_intact(intact: bool) -> f64 {
    if intact {
        1.0
    } else {
        0.0
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Constants 0.0/1.0 are exact float values in tests.
mod tests {
    use super::*;

    #[test]
    fn resume_correctness_full_and_partial() {
        assert!((resume_correctness(4, 4, 0).expect("ok") - 1.0).abs() < 1e-9);
        assert!((resume_correctness(4, 2, 0).expect("ok") - 0.5).abs() < 1e-9);
    }

    #[test]
    fn resume_correctness_side_effect_forces_zero() {
        assert_eq!(resume_correctness(4, 4, 1).expect("ok"), 0.0);
    }

    #[test]
    fn resume_correctness_rejects_zero_steps() {
        assert!(resume_correctness(0, 0, 0).is_err());
    }

    #[test]
    fn recall_at_k_basic() {
        assert!((recall_at_k(10, 9).expect("ok") - 0.9).abs() < 1e-9);
        assert!(recall_at_k(0, 0).is_err());
    }

    #[test]
    fn dedup_precision_no_merges_is_one() {
        assert_eq!(dedup_precision(0, 0).expect("ok"), 1.0);
        assert!((dedup_precision(3, 1).expect("ok") - 0.75).abs() < 1e-9);
    }

    #[test]
    fn protected_core_maps_bool() {
        assert_eq!(protected_core_intact(true), 1.0);
        assert_eq!(protected_core_intact(false), 0.0);
    }
}
