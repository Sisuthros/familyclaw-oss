//! [`ComparativeScorecard`]: a **head-to-head comparison** of two subjects' results.
//!
//! This is the public artifact for the `surpass` proof: the same
//! deterministic scenario suite is run against **both** subjects
//! ([`FamilyClawSubject`](crate::FamilyClawSubject) and
//! [`MarkdownFileSubject`](crate::subjects::MarkdownFileSubject)), and the
//! results are rendered side by side in a two-column table per scenario. The
//! reader sees at a glance where FamilyClaw passes and the baseline fails.
//!
//! ## Honesty warning (hard requirement)
//! The baseline is NOT a real OpenClaw or Hermes Agent — it is a
//! *competitor-SHAPED model* (a truncating `MEMORY.md` plus side effects
//! that re-run on restart). The comparison report's **header states this
//! plainly**, so no one can read it as a claim about any real product's
//! internals. The modeled behaviors are documented failure modes — not
//! exaggerations.
//!
//! ## Reproducibility (design §2.2, §6)
//! Both scorecards are built with an **injected** clock ([`Timestamp`]) —
//! the system clock is never read. Scenario results are sorted by ID
//! ([`Scorecard::new`]), and the comparison joins them by ID, so the same
//! input produces byte-for-byte identical markdown on every run.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use familyclaw_core::{time, Timestamp};

use crate::scenario::ScenarioResult;
use crate::scorecard::Scorecard;

/// Keys for the metrics surfaced in the comparison table (design §3: these
/// are what distinguish durable replay from the truncating baseline).
///
/// If a scenario doesn't record one of these, the column shows `—`.
const KEY_METRICS: [&str; 4] = [
    // S1: how many side effects were re-run on restart (target: 0).
    "side_effect_overcount",
    // S1: did work resume from exactly the right step (1.0 = perfect).
    "resume_correctness",
    // S2: did identity anchors survive after 90 days (retention).
    "anchor_retention_90d",
    // S2: did the subject's own recall find the expected hits.
    "subject_recall_hits",
];

/// A head-to-head comparison of two subjects' scorecards.
///
/// `familyclaw` and `baseline` were run with the **same** scenario suite and
/// the **same** injected clock. [`to_markdown`](Self::to_markdown) renders
/// an honestly labeled comparison report.
#[derive(Debug, Clone, PartialEq)]
pub struct ComparativeScorecard {
    /// The FamilyClaw subject's scorecard (first column).
    pub familyclaw: Scorecard,
    /// The competitor-shaped baseline's scorecard (second column).
    pub baseline: Scorecard,
    /// The injected reference instant — NOT the system clock (reproducibility).
    pub clock: Timestamp,
}

/// A single metric's comparison row (FamilyClaw vs baseline) for rendering.
struct MetricRow {
    /// The metric's key (e.g. `side_effect_overcount`).
    key: String,
    /// The FamilyClaw value, formatted, or `—` if missing.
    familyclaw: String,
    /// The baseline value, formatted, or `—` if missing.
    baseline: String,
}

impl ComparativeScorecard {
    /// Builds a comparison from two scorecards and an injected clock.
    ///
    /// Both scorecards' scenarios are already sorted by ID (guaranteed by
    /// [`Scorecard::new`]), so joining by ID is deterministic.
    #[must_use]
    pub fn new(familyclaw: Scorecard, baseline: Scorecard, clock: Timestamp) -> Self {
        Self {
            familyclaw,
            baseline,
            clock,
        }
    }

    /// Looks up a scenario result by ID in a scorecard (linear search; there
    /// are only a handful of scenarios).
    fn find<'a>(card: &'a Scorecard, id: &str) -> Option<&'a ScenarioResult> {
        card.scenarios.iter().find(|s| s.id == id)
    }

    /// Formats a single subject's pass/fail label for a scenario.
    fn outcome(result: Option<&ScenarioResult>) -> &'static str {
        match result {
            Some(r) if r.passed => "PASS",
            Some(_) => "FAIL",
            None => "—",
        }
    }

    /// Assembles a scenario's key metrics into comparison rows (deterministic
    /// order: [`KEY_METRICS`]).
    fn metric_rows(fc: Option<&ScenarioResult>, base: Option<&ScenarioResult>) -> Vec<MetricRow> {
        let fmt = |result: Option<&ScenarioResult>, key: &str| -> String {
            result
                .and_then(|r| r.metrics.get(key))
                .map_or_else(|| "—".to_string(), |v| format!("{v:.4}"))
        };
        KEY_METRICS
            .iter()
            .filter(|key| {
                // Only show a metric if at least one subject recorded it.
                fc.is_some_and(|r| r.metrics.contains_key(**key))
                    || base.is_some_and(|r| r.metrics.contains_key(**key))
            })
            .map(|key| MetricRow {
                key: (*key).to_string(),
                familyclaw: fmt(fc, key),
                baseline: fmt(base, key),
            })
            .collect()
    }

    /// Did FamilyClaw succeed on the S1 Crash Matrix scenario where the
    /// baseline fails — i.e. `side_effect_overcount: 0` vs `> 0`.
    ///
    /// This is the `surpass` proof's core claim, machine-checkable: durable
    /// replay runs side effects exactly once, the truncating baseline re-runs
    /// them. Returns `true` only if both subjects recorded the
    /// `side_effect_overcount` metric and FamilyClaw = 0 < baseline.
    #[must_use]
    pub fn familyclaw_wins_crash_matrix(&self) -> bool {
        let id = "s1_crash_matrix";
        let fc = Self::find(&self.familyclaw, id);
        let base = Self::find(&self.baseline, id);
        let metric = "side_effect_overcount";
        let (Some(fc), Some(base)) = (fc, base) else {
            return false;
        };
        let (Some(&fc_val), Some(&base_val)) = (fc.metrics.get(metric), base.metrics.get(metric))
        else {
            return false;
        };
        // FamilyClaw re-runs zero side effects; the baseline re-runs > 0,
        // and FamilyClaw passes the scenario where the baseline does not.
        fc_val == 0.0 && base_val > 0.0 && fc.passed && !base.passed
    }

    /// Renders the comparison as human-readable markdown (`COMPARISON.md`).
    ///
    /// Structure:
    /// 1. **Honesty header** — the baseline is a competitor-SHAPED model,
    ///    NOT a real OpenClaw/Hermes instance.
    /// 2. **Summary table** — overall result per subject.
    /// 3. **Per-scenario** — two-column PASS/FAIL + key metrics.
    ///
    /// The output is byte-for-byte deterministic: fields in a fixed order,
    /// scenarios by ID, metrics in `KEY_METRICS` order, clock from the
    /// injected value.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();

        // 1) Header + honesty warning (hard requirement).
        out.push_str("# FamilyClaw vs Baseline — Continuity Comparison\n\n");
        out.push_str(
            "> **Honesty note:** the baseline is a *competitor-SHAPED model* \
             (a `MEMORY.md` that truncates oldest-first + side effects re-run \
             on restart), **NOT** a real OpenClaw / Hermes Agent instance. It \
             models the documented failure modes those file-based memories \
             exhibit — it does not claim to be any real product's internals.\n\n",
        );
        let _ = writeln!(
            out,
            "- **Reference clock (injected):** {}",
            time::to_rfc3339(self.clock)
        );
        let _ = writeln!(out, "- **FamilyClaw subject:** {}", self.familyclaw.subject);
        let _ = writeln!(out, "- **Baseline subject:** {}\n", self.baseline.subject);

        // 2) Summary table: overall result head to head.
        out.push_str("## Summary\n\n");
        out.push_str("| Subject | Overall |\n|---------|---------|\n");
        let _ = writeln!(
            out,
            "| {} (FamilyClaw) | {} |",
            self.familyclaw.subject,
            if self.familyclaw.all_passed() {
                "PASS"
            } else {
                "FAIL"
            }
        );
        let _ = writeln!(
            out,
            "| {} (baseline) | {} |",
            self.baseline.subject,
            if self.baseline.all_passed() {
                "PASS"
            } else {
                "FAIL"
            }
        );
        out.push('\n');

        // 3) Per-scenario comparison. Scenario IDs are collected from both
        //    cards into a BTreeSet → deterministic alphabetical order.
        let ids: BTreeSet<&str> = self
            .familyclaw
            .scenarios
            .iter()
            .chain(self.baseline.scenarios.iter())
            .map(|s| s.id.as_str())
            .collect();

        for id in ids {
            let fc = Self::find(&self.familyclaw, id);
            let base = Self::find(&self.baseline, id);

            let _ = writeln!(out, "## {id}\n");
            out.push_str("| Dimension | FamilyClaw | Baseline |\n");
            out.push_str("|-----------|------------|----------|\n");
            let _ = writeln!(
                out,
                "| result | {} | {} |",
                Self::outcome(fc),
                Self::outcome(base)
            );
            for row in Self::metric_rows(fc, base) {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} |",
                    row.key, row.familyclaw, row.baseline
                );
            }
            out.push('\n');
        }

        // 4) Verdict — the machine-checkable core claim in prose.
        out.push_str("## Verdict\n\n");
        if self.familyclaw_wins_crash_matrix() {
            out.push_str(
                "On **S1 Crash Matrix**, FamilyClaw re-executes \
                 `side_effect_overcount: 0` side effects across every crash point \
                 and passes; the baseline re-runs `> 0` side effects on restart \
                 and fails. Durable replay plus the idempotency-keyed dispatch \
                 outbox dispatch each side effect **at most once** under a crash — \
                 a side effect never fires twice; a crash in the narrow \
                 intent-only window fails closed (zero or one execution, requiring \
                 recovery) rather than re-firing blindly. This is \
                 duplicate-prevention under crash, not a guarantee of universal \
                 exactly-once completion — the truncating file-memory baseline \
                 offers neither.\n",
            );
        } else {
            out.push_str(
                "S1 Crash Matrix comparison did not establish the expected \
                 FamilyClaw advantage in this run (see the table above).\n",
            );
        }

        out
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Constants 0.0/>0.0 are exact float values in tests.
mod tests {
    use super::*;
    use crate::scorecard::Scorecard;

    /// A fixed injected clock — constant in tests (reproducibility).
    fn fixed_clock() -> Timestamp {
        time::from_unix_secs(1_717_000_000).expect("valid clock")
    }

    /// Builds a FamilyClaw-style "winner" scorecard: S1 passes with zero
    /// side-effect overcount.
    fn familyclaw_card() -> Scorecard {
        let s1 = ScenarioResult::new("s1_crash_matrix", true)
            .with_metric("resume_correctness", 1.0)
            .with_metric("side_effect_overcount", 0.0)
            .with_metric("result_matches_baseline", 1.0);
        let s2 = ScenarioResult::new("s2_retention_curve", true)
            .with_metric("anchor_retention_90d", 1.0)
            .with_metric("subject_recall_hits", 4.0);
        Scorecard::new("familyclaw", vec![s1, s2], fixed_clock())
    }

    /// Builds a baseline "loser" scorecard: S1 fails because side effects
    /// are re-run.
    fn baseline_card() -> Scorecard {
        let s1 = ScenarioResult::new("s1_crash_matrix", false)
            .with_metric("resume_correctness", 0.0)
            .with_metric("side_effect_overcount", 12.0)
            .with_metric("result_matches_baseline", 0.0);
        let s2 = ScenarioResult::new("s2_retention_curve", false)
            .with_metric("anchor_retention_90d", 0.0)
            .with_metric("subject_recall_hits", 0.0);
        Scorecard::new("markdown-file-baseline", vec![s1, s2], fixed_clock())
    }

    #[test]
    fn markdown_is_byte_for_byte_reproducible() {
        let cmp_a = ComparativeScorecard::new(familyclaw_card(), baseline_card(), fixed_clock());
        let cmp_b = ComparativeScorecard::new(familyclaw_card(), baseline_card(), fixed_clock());
        assert_eq!(
            cmp_a.to_markdown(),
            cmp_b.to_markdown(),
            "same input → byte-for-byte identical comparison report"
        );
    }

    #[test]
    fn report_has_honesty_header() {
        let cmp = ComparativeScorecard::new(familyclaw_card(), baseline_card(), fixed_clock());
        let md = cmp.to_markdown();
        assert!(
            md.contains("competitor-SHAPED model"),
            "header honestly labels the baseline"
        );
        assert!(
            md.contains("NOT") && md.contains("OpenClaw") && md.contains("Hermes"),
            "report explicitly denies being a real product"
        );
    }

    #[test]
    fn familyclaw_passes_crash_matrix_where_baseline_fails() {
        let cmp = ComparativeScorecard::new(familyclaw_card(), baseline_card(), fixed_clock());
        assert!(
            cmp.familyclaw_wins_crash_matrix(),
            "FamilyClaw passes S1 (side_effect_overcount=0) where the \
             baseline fails (side_effect_overcount>0)"
        );

        // And the same shows up in the rendered report.
        let md = cmp.to_markdown();
        // On the S1 row FamilyClaw=0.0000, baseline=12.0000.
        assert!(md.contains("| side_effect_overcount | 0.0000 | 12.0000 |"));
        // The verdict states the advantage on honest grounds: at-most-once /
        // fail-closed, NOT a universal "exactly-once completion" promise.
        assert!(md.contains("at most once"));
        assert!(md.contains("not a guarantee of universal"));
        assert!(!md.contains("each side effect exactly once"));
    }

    #[test]
    fn wins_is_false_when_baseline_also_zero() {
        // If the baseline does NOT re-run side effects, the claim doesn't hold.
        let weak_baseline = Scorecard::new(
            "markdown-file-baseline",
            vec![ScenarioResult::new("s1_crash_matrix", true)
                .with_metric("side_effect_overcount", 0.0)],
            fixed_clock(),
        );
        let cmp = ComparativeScorecard::new(familyclaw_card(), weak_baseline, fixed_clock());
        assert!(
            !cmp.familyclaw_wins_crash_matrix(),
            "without a side-effect overcount on the baseline, the advantage claim doesn't hold"
        );
    }

    #[test]
    fn missing_metric_renders_as_dash() {
        // A scenario with no key metrics → columns show '—' or are omitted.
        let bare_fc = Scorecard::new(
            "familyclaw",
            vec![ScenarioResult::new("s9_bare", true)],
            fixed_clock(),
        );
        let bare_base = Scorecard::new(
            "markdown-file-baseline",
            vec![ScenarioResult::new("s9_bare", false)],
            fixed_clock(),
        );
        let cmp = ComparativeScorecard::new(bare_fc, bare_base, fixed_clock());
        let md = cmp.to_markdown();
        // The pass/fail result always shows even without metrics.
        assert!(md.contains("| result | PASS | FAIL |"));
    }
}
