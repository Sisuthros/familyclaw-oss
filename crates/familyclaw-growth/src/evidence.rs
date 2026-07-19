//! Replay-proven **promotion evidence** (WP2) — Layer A, OSS.
//!
//! The classic critique of a self-improving agent: it "always thinks it
//! succeeded". A verdict based on the agent's own self-assessment is not a
//! provable comparison. This module addresses that by giving a proposal a
//! **deterministic, serializable proof** of improvement: a comparison of two
//! timelines (baseline vs. candidate) via [`TimeMachine::diff`], from which a
//! *caller-supplied* metric decides whether an improvement occurred — and
//! **the evidence itself records how the verdict was reached**, not just a
//! boolean.
//!
//! ## Relationship to the proposal stack (a design decision)
//!
//! This module is **purely additive** and does not touch the
//! [`crate::Proposal`] structure. Why evidence was not added as a field on
//! `Proposal`:
//!
//! - `Proposal`'s [`content_hash`](crate::Proposal::content_hash) is a safety
//!   gate (TOCTOU drift protection): approval binds to the proposal's exact
//!   content. A new field would change the canonical content view and thus
//!   the hash of *every* existing proposal — old reviews and serialized
//!   proposals would stop matching (a serde compatibility break).
//! - Evidence is an *attachment* to a proposal, not part of its descriptive
//!   content: the same proposal can receive new evidence without its
//!   identity or reviewed content changing.
//!
//! For that reason, evidence is kept in a **parallel structure**
//! ([`EvidenceLedger`]), keyed by the proposal's
//! [`ProposalId`](crate::ProposalId). This keeps the core untouched and
//! structurally free of any apply path.
//!
//! ## Fail-closed
//!
//! [`evaluate_for_approval`] NEVER approves anything — it **only states**
//! whether the evidence requirements are met. Missing evidence, a regression
//! (`improved == false`), or an empty comparison →
//! [`EvidenceVerdict::insufficient`]. This does not replace the human/
//! operator approval gate ([`crate::ProposalStore::approve`]); it complements
//! it: the evidence requirement is a *precondition* for approval, not the
//! approval itself.

use std::collections::HashMap;

use familyclaw_durable::{Journal, Result as DurableResult, TimeMachine, Timeline, TimelineDiff};
use serde::{Deserialize, Serialize};

use crate::ProposalId;

/// A caller-supplied improvement metric: **how** a verdict of "improved /
/// did not improve" is derived from a diff of two timelines.
///
/// The metric is deliberately explicit and recorded, so the evidence
/// documents *on what basis* an improvement was determined — not just a
/// boolean. This avoids the "agent thinks it succeeded" trap: the basis for
/// the verdict is always readable from the evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImprovementMetric {
    /// Improvement = the candidate produced **more completed steps** than
    /// the baseline (with at least one step compared). A simple,
    /// deterministic default metric.
    MoreCompletedSteps,
    /// Improvement = the candidate changed exactly the **expected number**
    /// of steps and did not diverge by name (a targeted fix, not wild
    /// drift).
    ExactChangedCount {
        /// The expected number of changed steps.
        expected: usize,
    },
    /// Improvement = the timelines **did not diverge by name** (same step
    /// skeleton) and at least one step changed (something actually
    /// happened).
    NoDivergenceWithChange,
}

impl ImprovementMetric {
    /// Runs the metric over the baseline and candidate timelines and
    /// produces a verdict along with a human-readable justification.
    ///
    /// Returns `(improved, verdict_reason)`. The justification always
    /// explains *why* the verdict is what it is — including when no
    /// improvement was found.
    fn evaluate(
        &self,
        baseline: &Timeline,
        candidate: &Timeline,
        diff: &TimelineDiff,
    ) -> (bool, String) {
        match self {
            ImprovementMetric::MoreCompletedSteps => {
                let base_ok = completed_count(baseline);
                let cand_ok = completed_count(candidate);
                let improved = cand_ok > base_ok;
                let reason = format!(
                    "MoreCompletedSteps: candidate completed {cand_ok} step(s) vs baseline \
                     {base_ok} → improved={improved}"
                );
                (improved, reason)
            }
            ImprovementMetric::ExactChangedCount { expected } => {
                let changed = diff.changed_count();
                let diverged = diff.first_divergence();
                let improved = changed == *expected && diverged.is_none();
                let reason = format!(
                    "ExactChangedCount: expected {expected} changed step(s), observed {changed}; \
                     first_divergence={diverged:?} → improved={improved}"
                );
                (improved, reason)
            }
            ImprovementMetric::NoDivergenceWithChange => {
                let diverged = diff.first_divergence();
                let changed = diff.changed_count();
                let improved = diverged.is_none() && changed > 0;
                let reason = format!(
                    "NoDivergenceWithChange: first_divergence={diverged:?}, changed={changed} → \
                     improved={improved}"
                );
                (improved, reason)
            }
        }
    }
}

/// The number of completed (Completed) steps in a timeline.
fn completed_count(timeline: &Timeline) -> usize {
    timeline
        .steps
        .iter()
        .filter(|s| s.outcome.is_completed())
        .count()
}

/// A deterministic, serializable **proof** comparing two timelines.
///
/// Captures the [`TimelineDiff`] comparison of the baseline and candidate
/// timelines, along with summary fields and an explicit `improved` verdict
/// with its `verdict_reason` justification. The evidence is **inert data**:
/// it does not apply or approve anything, it merely *proves* on what basis
/// the candidate was (or was not) an improvement.
///
/// Constructed via [`ReplayEvidence::from_journals`] or
/// [`ReplayEvidence::from_timelines`]. Both are **read-only** with respect to
/// the journals/timelines involved — no existing timeline is modified.
///
/// ## Why `diff` is `serde_json::Value` instead of `TimelineDiff`
///
/// [`TimelineDiff`] (and its `StepDiff`/`StepOutcome`) derive *only*
/// [`Serialize`] in the durable crate, not [`Deserialize`]. For the evidence
/// to be **fully roundtrippable** (store + reload) without touching the
/// durable crate's types, we keep the diff in its **serialized form**
/// ([`serde_json::Value`]). The format is deterministic and human-readable,
/// and the original [`TimelineDiff`] can always be re-derived by rerunning
/// the comparison from the source timelines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayEvidence {
    /// The improvement metric used (how the verdict was reached).
    pub metric: ImprovementMetric,
    /// The deterministic diff produced by the comparison
    /// (baseline vs. candidate), **serialized**. Stored as
    /// [`serde_json::Value`]; see the type doc for why.
    pub diff: serde_json::Value,
    /// The number of steps compared (the step pairs/remainders present in
    /// the diff).
    pub steps_compared: usize,
    /// How many steps changed ([`TimelineDiff::changed_count`]).
    pub changed_count: usize,
    /// How many steps were present on only one timeline
    /// ([`TimelineDiff::tail_count`]).
    pub tail_count: usize,
    /// The first point where the timelines diverged by name, if any.
    pub first_divergence: Option<usize>,
    /// The explicit verdict: whether the candidate was an improvement.
    pub improved: bool,
    /// A human-readable justification for the verdict — *how* it was
    /// reached.
    pub verdict_reason: String,
}

impl ReplayEvidence {
    /// Derives evidence from two already-read timelines using the given
    /// metric. Purely computational; does not touch the sources.
    #[must_use]
    pub fn from_timelines(
        baseline: &Timeline,
        candidate: &Timeline,
        metric: ImprovementMetric,
    ) -> Self {
        let diff = TimelineDiff::from_timelines(baseline, candidate);
        let (improved, verdict_reason) = metric.evaluate(baseline, candidate, &diff);
        // The summary fields are always read from the *real* TimelineDiff, so
        // the verdict is independent of whether serialization succeeds. Only
        // the stored diff value is the serialized form; in practice
        // TimelineDiff serialization does not fail (it's plain data), but if
        // it ever did, we store `null` (fail-closed: no panic, summaries are
        // preserved).
        let diff_value = serde_json::to_value(&diff).unwrap_or(serde_json::Value::Null);
        Self {
            metric,
            steps_compared: diff.steps.len(),
            changed_count: diff.changed_count(),
            tail_count: diff.tail_count(),
            first_divergence: diff.first_divergence(),
            diff: diff_value,
            improved,
            verdict_reason,
        }
    }

    /// Derives evidence from two journals using the given metric: reads both
    /// as timelines (in the style of [`TimeMachine::diff`]) and compares
    /// them.
    ///
    /// `baseline` is the starting point of the comparison, and `candidate`
    /// (e.g. a forked counterfactual) is the continuation compared against
    /// it. Neither journal is modified.
    ///
    /// # Errors
    /// Propagates a read error from either journal
    /// ([`familyclaw_durable::DurableError`]).
    pub fn from_journals<A: Journal, B: Journal>(
        baseline: &A,
        candidate: &B,
        metric: ImprovementMetric,
    ) -> DurableResult<Self> {
        // Same read approach as TimeMachine::diff — kept consistent.
        let base_tl = TimeMachine::inspect(baseline)?;
        let cand_tl = TimeMachine::inspect(candidate)?;
        Ok(Self::from_timelines(&base_tl, &cand_tl, metric))
    }

    /// Whether the comparison is **empty** (zero steps compared) → the
    /// evidence proves nothing. Fail-closed evaluation treats this as
    /// insufficient.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps_compared == 0
    }
}

/// The verdict on whether the evidence attached to a proposal meets the
/// **evidence requirements** for approval. This is NOT an approval — it is
/// the *precondition* for the gate.
///
/// Fail-closed: any uncertainty (missing evidence, regression, empty
/// comparison) produces [`EvidenceVerdict::Insufficient`] with a reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceVerdict {
    /// The evidence requirements are met: the attached evidence is
    /// non-empty and its verdict is `improved == true`. **Still not an
    /// approval** — the human/operator approval gate remains on the path.
    RequirementsMet,
    /// The evidence requirements are NOT met → not approvable
    /// (deny-by-default).
    Insufficient {
        /// A human-readable reason why the evidence is insufficient (audit
        /// trail).
        reason: String,
    },
}

impl EvidenceVerdict {
    /// Builds an insufficient verdict with the given reason.
    fn insufficient(reason: impl Into<String>) -> Self {
        Self::Insufficient {
            reason: reason.into(),
        }
    }

    /// Whether the evidence requirements were met.
    #[must_use]
    pub const fn is_met(&self) -> bool {
        matches!(self, EvidenceVerdict::RequirementsMet)
    }
}

/// Evaluates whether the given evidence meets the **evidence requirements**
/// for approval. **Fail-closed**: this function does not approve anything —
/// it only states whether the precondition for approval (proven
/// improvement) exists.
///
/// Insufficient (→ [`EvidenceVerdict::Insufficient`]) when:
/// - no evidence is attached (`None`),
/// - the evidence is empty (0 steps compared),
/// - the evidence's verdict is `improved == false` (regression or no
///   improvement).
///
/// Only when evidence exists, is non-empty, and `improved == true` is
/// [`EvidenceVerdict::RequirementsMet`] returned. Even then, the actual
/// approval is a separate, human/operator-performed step
/// ([`crate::ProposalStore::approve`]) — this function does not replace it.
#[must_use]
pub fn evaluate_for_approval(evidence: Option<&ReplayEvidence>) -> EvidenceVerdict {
    let Some(evidence) = evidence else {
        return EvidenceVerdict::insufficient(
            "no replay evidence attached: cannot prove improvement → not approvable \
             (deny-by-default)",
        );
    };
    if evidence.is_empty() {
        return EvidenceVerdict::insufficient(
            "replay evidence compared 0 steps: nothing was proven → not approvable \
             (deny-by-default)",
        );
    }
    if !evidence.improved {
        return EvidenceVerdict::insufficient(format!(
            "replay evidence verdict is improved=false → not approvable (deny-by-default): {}",
            evidence.verdict_reason
        ));
    }
    EvidenceVerdict::RequirementsMet
}

/// A parallel, additive **evidence ledger**: attaches replay evidence to
/// proposals via their [`ProposalId`], without touching the
/// [`crate::Proposal`] structure (and therefore not its content hash).
///
/// Purely record-keeping/query — **no apply path**, just like
/// [`crate::ProposalStore`]. Attaching evidence does not approve or apply
/// anything; evaluation happens via the [`evaluate_for_approval`] function.
#[derive(Debug, Default, Clone)]
pub struct EvidenceLedger {
    evidence: HashMap<ProposalId, ReplayEvidence>,
}

impl EvidenceLedger {
    /// Creates an empty evidence ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches (or replaces) the replay evidence for a proposal. Returns
    /// any previous evidence. Does NOT approve or apply anything.
    pub fn attach(
        &mut self,
        proposal_id: ProposalId,
        evidence: ReplayEvidence,
    ) -> Option<ReplayEvidence> {
        self.evidence.insert(proposal_id, evidence)
    }

    /// Retrieves the evidence attached to a proposal, if any.
    #[must_use]
    pub fn get(&self, proposal_id: ProposalId) -> Option<&ReplayEvidence> {
        self.evidence.get(&proposal_id)
    }

    /// Evaluates the evidence attached to a proposal against the approval
    /// evidence requirements (fail-closed). Missing evidence →
    /// [`EvidenceVerdict::Insufficient`].
    #[must_use]
    pub fn evaluate(&self, proposal_id: ProposalId) -> EvidenceVerdict {
        evaluate_for_approval(self.get(proposal_id))
    }

    /// The number of attached evidence entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.evidence.len()
    }

    /// Whether the ledger is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.evidence.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_durable::{DurableContext, InMemoryJournal};

    /// Helper: a two-step run (load → decide) with the given values.
    fn two_step_run(load: i64, decide: i64) -> InMemoryJournal {
        let mut ctx = DurableContext::new(InMemoryJournal::new()).expect("ctx");
        let a: i64 = ctx.step("load", || Ok(load)).expect("load");
        let _b: i64 = ctx.step("decide", || Ok(a + decide)).expect("decide");
        ctx.finish()
    }

    // ---------- ReplayEvidence derivation ----------

    #[test]
    fn evidence_from_timelines_records_how_verdict_was_reached() {
        // Baseline: one failed step. Candidate: the same step succeeds.
        let mut base = DurableContext::new(InMemoryJournal::new()).expect("base");
        let _ = base.step::<i32, _>("risky", || Err("boom".to_string()));
        let base = base.finish();

        let mut cand = DurableContext::new(InMemoryJournal::new()).expect("cand");
        let _ = cand.step("risky", || Ok::<_, String>(1)).expect("ok");
        let cand = cand.finish();

        let base_tl = TimeMachine::inspect(&base).expect("base tl");
        let cand_tl = TimeMachine::inspect(&cand).expect("cand tl");
        let ev = ReplayEvidence::from_timelines(
            &base_tl,
            &cand_tl,
            ImprovementMetric::MoreCompletedSteps,
        );

        assert!(ev.improved, "kandidaatti onnistui, baseline ei");
        assert_eq!(ev.steps_compared, 1);
        assert_eq!(ev.changed_count, 1);
        assert!(
            ev.verdict_reason.contains("MoreCompletedSteps"),
            "perustelu kertoo miten verdiktiin päädyttiin: {}",
            ev.verdict_reason
        );
        assert!(!ev.is_empty());
    }

    #[test]
    fn evidence_from_journals_matches_time_machine_diff() {
        let base = two_step_run(10, 5);
        // Candidate: same load, different decide → one changed step.
        let cand = two_step_run(10, 7);

        let ev =
            ReplayEvidence::from_journals(&base, &cand, ImprovementMetric::NoDivergenceWithChange)
                .expect("evidence");

        // Same read approach as TimeMachine::diff — the stored diff is its
        // serialized form.
        let direct = TimeMachine::diff(&base, &cand).expect("diff");
        let direct_value = serde_json::to_value(&direct).expect("serialize diff");
        assert_eq!(ev.diff, direct_value);
        assert_eq!(ev.changed_count, 1);
        assert_eq!(ev.first_divergence, None);
        assert!(ev.improved, "ei erkaantumista + yksi muutos = parannus");
    }

    // ---------- evaluate_for_approval: fail-closed gate ----------

    #[test]
    fn no_evidence_is_insufficient() {
        let verdict = evaluate_for_approval(None);
        assert!(!verdict.is_met());
        match verdict {
            EvidenceVerdict::Insufficient { reason } => {
                assert!(reason.contains("no replay evidence"));
                assert!(reason.contains("deny-by-default"));
            }
            EvidenceVerdict::RequirementsMet => panic!("missing evidence must be insufficient"),
        }
    }

    #[test]
    fn improvement_meets_requirements() {
        let base = two_step_run(10, 5);
        let cand = two_step_run(10, 9);
        let ev =
            ReplayEvidence::from_journals(&base, &cand, ImprovementMetric::NoDivergenceWithChange)
                .expect("evidence");
        assert!(ev.improved);
        let verdict = evaluate_for_approval(Some(&ev));
        assert_eq!(verdict, EvidenceVerdict::RequirementsMet);
        assert!(verdict.is_met());
    }

    #[test]
    fn regression_is_insufficient() {
        // Identical timelines → NoDivergenceWithChange: 0 changes → no improvement.
        let base = two_step_run(10, 5);
        let cand = two_step_run(10, 5);
        let ev =
            ReplayEvidence::from_journals(&base, &cand, ImprovementMetric::NoDivergenceWithChange)
                .expect("evidence");
        assert!(!ev.improved, "ei muutosta → ei parannusta");
        assert!(!ev.is_empty(), "askelia kuitenkin verrattiin");

        let verdict = evaluate_for_approval(Some(&ev));
        assert!(!verdict.is_met());
        match verdict {
            EvidenceVerdict::Insufficient { reason } => {
                assert!(reason.contains("improved=false"));
                assert!(reason.contains("deny-by-default"));
            }
            EvidenceVerdict::RequirementsMet => panic!("regression must be insufficient"),
        }
    }

    #[test]
    fn empty_comparison_is_insufficient() {
        // Two empty journals → 0 steps compared.
        let base = InMemoryJournal::new();
        let cand = InMemoryJournal::new();
        let ev = ReplayEvidence::from_journals(&base, &cand, ImprovementMetric::MoreCompletedSteps)
            .expect("evidence");
        assert!(ev.is_empty());
        assert_eq!(ev.steps_compared, 0);

        let verdict = evaluate_for_approval(Some(&ev));
        assert!(!verdict.is_met());
        match verdict {
            EvidenceVerdict::Insufficient { reason } => {
                assert!(reason.contains("0 steps"));
            }
            EvidenceVerdict::RequirementsMet => panic!("empty comparison must be insufficient"),
        }
    }

    // ---------- serde roundtrip ----------

    #[test]
    fn replay_evidence_roundtrips_json() {
        let base = two_step_run(10, 5);
        let cand = two_step_run(10, 8);
        let ev = ReplayEvidence::from_journals(
            &base,
            &cand,
            ImprovementMetric::ExactChangedCount { expected: 1 },
        )
        .expect("evidence");

        let json = serde_json::to_string(&ev).expect("serialize");
        let back: ReplayEvidence = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ev, back);
        // Verdict + justification survive the round trip.
        assert_eq!(ev.improved, back.improved);
        assert_eq!(ev.verdict_reason, back.verdict_reason);
    }

    #[test]
    fn evidence_verdict_roundtrips_json() {
        let met = EvidenceVerdict::RequirementsMet;
        let json = serde_json::to_string(&met).expect("serialize");
        let back: EvidenceVerdict = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(met, back);

        let insufficient = EvidenceVerdict::insufficient("test reason");
        let json = serde_json::to_string(&insufficient).expect("serialize");
        let back: EvidenceVerdict = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(insufficient, back);
    }

    // ---------- EvidenceLedger: parallel additive structure ----------

    #[test]
    fn ledger_attaches_and_evaluates_without_touching_proposal() {
        let base = two_step_run(10, 5);
        let cand = two_step_run(10, 9);
        let ev =
            ReplayEvidence::from_journals(&base, &cand, ImprovementMetric::NoDivergenceWithChange)
                .expect("evidence");

        let mut ledger = EvidenceLedger::new();
        let id = ProposalId::new();

        // Before attaching: fail-closed (no evidence).
        assert!(!ledger.evaluate(id).is_met());
        assert!(ledger.is_empty());

        let prev = ledger.attach(id, ev.clone());
        assert!(prev.is_none());
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.get(id), Some(&ev));
        assert!(
            ledger.evaluate(id).is_met(),
            "parannus → vaatimukset täyttyvät"
        );
    }

    /// Core test (end-to-end): builds two real journals with
    /// [`DurableContext`] — a baseline and an improved counterfactual — and
    /// derives evidence via a [`TimeMachine::diff`] comparison. Proves that
    /// the whole chain (run → diff → evidence → fail-closed evaluation)
    /// works with real timelines.
    #[test]
    fn end_to_end_baseline_vs_improved_counterfactual() {
        // Baseline: a three-step run where "act" fails (a bad outcome).
        let mut base = DurableContext::new(InMemoryJournal::new()).expect("base");
        let amount: i64 = base.step("load", || Ok(100)).expect("load");
        let approved: i64 = base.step("decide", || Ok(amount * 2)).expect("decide");
        let _ = base.step::<String, _>("act", move || {
            let _ = approved;
            Err("baseline act failed".to_string())
        });
        let baseline = base.finish();

        // Fork before the "act" step and run a fixed continuation where "act" succeeds.
        let fork = TimeMachine::fork(&baseline, 2).expect("fork");
        let mut cand = DurableContext::new(fork).expect("cand ctx");
        let amount: i64 = cand.step("load", || Ok(0)).expect("load replay"); // replayed from the log
        let approved: i64 = cand.step("decide", || Ok(0)).expect("decide replay"); // replayed
        assert_eq!((amount, approved), (100, 200), "prefiksi palautuu lokista");
        let _receipt: String = cand
            .step("act", move || Ok(format!("sent:{approved}")))
            .expect("act candidate");
        let candidate = cand.finish();

        // Derive evidence: the candidate has more completed steps than the baseline.
        let ev = ReplayEvidence::from_journals(
            &baseline,
            &candidate,
            ImprovementMetric::MoreCompletedSteps,
        )
        .expect("evidence");

        assert_eq!(ev.steps_compared, 3);
        assert_eq!(
            ev.first_divergence, None,
            "askelrunko ei erkaantunut nimeltään"
        );
        assert_eq!(ev.changed_count, 1, "vain act muuttui (fail → ok)");
        assert!(ev.improved, "3 onnistunutta vs 2 → parannus");
        assert!(
            ev.verdict_reason.contains("completed 3"),
            "perustelu näyttää mittaustuloksen: {}",
            ev.verdict_reason
        );

        // Fail-closed gate: evidence requirements are met (but NOT an approval).
        let mut ledger = EvidenceLedger::new();
        let id = ProposalId::new();
        ledger.attach(id, ev);
        assert!(ledger.evaluate(id).is_met());

        // The baseline did not change during the comparison (append-only invariant).
        let baseline_after = TimeMachine::inspect(&baseline).expect("inspect");
        assert_eq!(baseline_after.len(), 3);
    }
}
