//! [`DurableContext`] — the deterministic replay `step` API.
//!
//! This is the structural solution to the family's pain point #1 (memory
//! discontinuity) (design §2.1). The workflow is wrapped into steps
//! ([`step`](DurableContext::step)). When a context is built on top of an
//! existing journal, steps that already ran are **restored from the log
//! without re-running their closures** — meaning side effects do not recur,
//! but the result is the same. After a crash, the workflow resumes exactly
//! where it left off.
//!
//! ## The determinism invariant
//! The code must produce the same steps (same name, same order) on every
//! run. If replaying code requests a step whose name does not match the one
//! recorded in the journal at that position, [`step`](DurableContext::step)
//! returns [`DurableError::NondeterministicReplay`] instead of silently
//! continuing incorrectly.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::entry::{EntryKind, JournalEntry, StepId};
use crate::error::{DurableError, Result};
use crate::journal::Journal;

/// Execution context for deterministic replay over a single journal.
///
/// Generic over the journal implementation `J`, so the same logic works with
/// both [`crate::InMemoryJournal`] and [`crate::FileJournal`] as the backend.
#[derive(Debug)]
pub struct DurableContext<J: Journal> {
    journal: J,
    /// Previously recorded rows over which replay advances. Only rows tied
    /// to a step (StepCompleted/StepFailed) — snapshots AND markers are
    /// filtered out, since they are not `step` calls.
    replay: Vec<JournalEntry>,
    /// How many `step` calls have already been made on this context. Serves
    /// both as the replay cursor and as the next step's sequence position.
    cursor: usize,
}

impl<J: Journal> DurableContext<J> {
    /// Builds a context on top of a journal, loading prior steps as the
    /// replay basis.
    ///
    /// # Errors
    /// Propagates the error if reading the journal fails
    /// (e.g. [`DurableError::CorruptEntry`]).
    pub fn new(journal: J) -> Result<Self> {
        let all = journal.replay_all()?;
        // Keep only step rows (StepCompleted/StepFailed) for the replay
        // cursor. Snapshots (an optimization) and markers (e.g. dreaming-phase
        // contradiction annotations) are NOT `step` calls, so they do not
        // consume the cursor — this way the same shared log can carry both
        // without a marker row appearing as a workflow step and triggering a
        // NondeterministicReplay error.
        let replay: Vec<JournalEntry> = all.into_iter().filter(|e| e.kind.is_step()).collect();
        Ok(Self {
            journal,
            replay,
            cursor: 0,
        })
    }

    /// Whether the context is currently replaying previously recorded steps.
    ///
    /// `true` for as long as the cursor has not passed the recorded rows.
    #[must_use]
    pub fn is_replaying(&self) -> bool {
        self.cursor < self.replay.len()
    }

    /// How many steps have already run or been replayed.
    #[must_use]
    pub fn steps_taken(&self) -> usize {
        self.cursor
    }

    /// **Skips the entire replay** — moves the cursor to the end of the
    /// recorded rows **without** re-running them, so that
    /// [`is_replaying`](Self::is_replaying) returns `false` and the next
    /// [`step`](Self::step) takes the fresh-run branch.
    ///
    /// This is the **live-resume** primitive: when a context is built on top
    /// of an existing journal BUT the caller does not intend to feed the
    /// history back in (e.g. a gateway restart that serves ONLY new
    /// messages), the prior replay history should not be replayed step by
    /// step. Without this, the first live step (a new name, e.g. `turn-{N}`)
    /// would land in the still-open replay branch and fail with
    /// [`DurableError::NondeterministicReplay`], because the recorded name
    /// (`turn-0`) would not match.
    ///
    /// The next fresh step is assigned the sequence position `replay.len()`
    /// (continuing after the log), and no recorded side effect is re-run.
    ///
    /// Counterpart: in-order re-feeding (the continuity daemon, replay tests)
    /// does NOT call this — it specifically wants to replay history step by
    /// step.
    pub fn fast_forward_replay(&mut self) {
        self.cursor = self.replay.len();
    }

    /// The sequence position of the next step.
    #[must_use]
    pub fn next_step_id(&self) -> StepId {
        StepId::new(self.cursor as u64)
    }

    /// Executes the named step with run-once-and-only-once semantics.
    ///
    /// - **Fresh run:** the closure `f` runs, its result is serialized and
    ///   written to the journal before returning.
    /// - **Replay:** if a row is already recorded at this position, the
    ///   closure `f` is **not run** — the recorded result is parsed and
    ///   returned (or the recorded error is returned).
    ///
    /// This guarantees that a step's side effects (a network call, a file
    /// write) happen exactly once over the workflow's entire lifecycle, even
    /// if the process crashes and restarts mid-way.
    ///
    /// # Errors
    /// - [`DurableError::NondeterministicReplay`] if `name` does not match
    ///   the step recorded in the journal at this position.
    /// - [`DurableError::StepFailed`] if the closure returned an error (fresh
    ///   run) or the recorded row was a failure (replay).
    /// - [`DurableError::Serde`] if serializing/parsing the result fails.
    /// - [`DurableError::Io`] if writing to the journal fails.
    pub fn step<T, F>(&mut self, name: &str, f: F) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> std::result::Result<T, String>,
    {
        let index = self.cursor as u64;

        // Replay branch: a row is already recorded at this position.
        if let Some(entry) = self.replay.get(self.cursor) {
            let recorded_name = entry.step_name().unwrap_or_default();
            if recorded_name != name {
                return Err(DurableError::NondeterministicReplay {
                    index,
                    expected: name.to_string(),
                    found: recorded_name.to_string(),
                });
            }
            let result = match &entry.kind {
                EntryKind::StepCompleted { output, .. } => {
                    let value: T = serde_json::from_value(output.clone())?;
                    Ok(value)
                }
                EntryKind::StepFailed { error, .. } => {
                    Err(DurableError::step_failed(name, error.clone()))
                }
                // The replay vector contains only step rows (`is_step`), so
                // snapshots/markers (and any future non-step kinds) have
                // already been filtered out in `new`. This branch should
                // never be reached — but it's still handled without panicking.
                other => Err(DurableError::NondeterministicReplay {
                    index,
                    expected: name.to_string(),
                    found: format!("<non-step entry: {}>", non_step_label(other)),
                }),
            };
            self.cursor += 1;
            return result;
        }

        // Fresh-run branch: the closure runs once.
        let step_id = StepId::new(index);
        match f() {
            Ok(value) => {
                let output = serde_json::to_value(&value)?;
                self.journal
                    .append(JournalEntry::completed(step_id, name, output))?;
                self.cursor += 1;
                Ok(value)
            }
            Err(message) => {
                // Record the failure so replay returns the same error without
                // re-running side effects.
                self.journal
                    .append(JournalEntry::failed(step_id, name, message.clone()))?;
                self.cursor += 1;
                Err(DurableError::step_failed(name, message))
            }
        }
    }

    /// Writes a snapshot of the current state at the current sequence position.
    ///
    /// A snapshot does not consume the `step` cursor and does not interrupt
    /// replay — it is an additional entry in the log for auditing/optimization.
    ///
    /// # Errors
    /// [`DurableError::Io`]/[`DurableError::Serde`] if the write fails.
    pub fn snapshot<S: Serialize>(&mut self, state: &S) -> Result<()> {
        let value = serde_json::to_value(state)?;
        self.journal.snapshot(self.next_step_id(), value)
    }

    /// Consumes the context and returns the underlying journal.
    ///
    /// Used when the workflow has finished running (or to simulate a
    /// "crash" in tests): the journal can be taken and a new context built
    /// on top of it for replay.
    #[must_use]
    pub fn finish(self) -> J {
        self.journal
    }

    /// Returns a reference to the underlying journal (e.g. to inspect rows
    /// in tests).
    #[must_use]
    pub fn journal(&self) -> &J {
        &self.journal
    }

    /// How many **top-level turns** (`turn-{n}`) are in the replay vector.
    ///
    /// The agent's turn handling records TWO steps per turn: the top-level
    /// `turn-{n}` and its sub-step `turn-{n}-think`. When the agent is built
    /// on top of an existing journal (e.g. a gateway restart), its
    /// `turn_counter` must be initialized with this number — otherwise the
    /// next LIVE turn would start at `turn-0`, land in the replay branch, and
    /// return the old result without processing the new message (restart
    /// muteness + memory loss).
    ///
    /// **Sub-steps are excluded** (`-think`): only names of the exact form
    /// `turn-{n}`, where `{n}` is a plain number, are counted. Returns the
    /// highest turn number **+ 1** (i.e. the next free turn slot), or `0` if
    /// there are no turns. `max+1` (rather than a plain counter) is resilient
    /// to gaps in the log.
    ///
    /// **This does NOT mutate the replay cursor** — it only reads the replay
    /// vector. The in-memory (non-persistent) path, where replay is empty,
    /// returns `0`.
    #[must_use]
    pub fn replayed_turn_count(&self) -> u64 {
        self.replay
            .iter()
            .filter_map(|e| e.step_name())
            .filter_map(parse_top_level_turn)
            .max()
            .map_or(0, |max| max + 1)
    }

    /// Checks whether a given step has already run.
    ///
    /// # Errors
    /// Returns an error if reading the journal fails.
    pub fn has_run_step(&self, name: &str) -> Result<bool> {
        // The replay vector contains only step rows (StepCompleted/StepFailed).
        for entry in &self.replay {
            if let Some(step_name) = entry.step_name() {
                if step_name == name {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

/// Parses the **top-level turn** number from a step name, if the name is
/// exactly `turn-{n}` (not a `turn-{n}-think` sub-step nor any other step).
///
/// Returns `Some(n)` only when `{n}` is a valid `u64` with no extra suffix;
/// otherwise `None`. This way `replayed_turn_count` counts each turn only
/// once, even though every turn also records a `-think` sub-step.
fn parse_top_level_turn(step_name: &str) -> Option<u64> {
    step_name.strip_prefix("turn-")?.parse::<u64>().ok()
}

/// A short diagnostic label for non-step row kinds (snapshot/marker/future kind).
///
/// Used only on the "should never happen" error path: the replay vector is
/// already filtered down to steps only, so this is in practice essentially
/// never called.
fn non_step_label(kind: &EntryKind) -> &'static str {
    if kind.is_snapshot() {
        "snapshot"
    } else if kind.is_marker() {
        "marker"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::InMemoryJournal;
    use std::cell::Cell;

    /// Helper: a fresh context on top of an empty in-memory journal.
    fn fresh() -> DurableContext<InMemoryJournal> {
        DurableContext::new(InMemoryJournal::new()).expect("new context")
    }

    /// A small RAII temp file without external crates.
    struct TempPath(std::path::PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "familyclaw-durable-ctx-{tag}-{}-{:?}.jsonl",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            let _ = std::fs::remove_file(&p);
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// A three-step test workflow: 10 → +5 → ×2 = 30. The counter records how
    /// many times the closures are ACTUALLY run (replay does not increment it).
    fn three_step_workflow<J: Journal>(
        ctx: &mut DurableContext<J>,
        effects: &Cell<u32>,
    ) -> Result<i32> {
        let a: i32 = ctx.step("step_a", || {
            effects.set(effects.get() + 1);
            Ok(10)
        })?;
        let b: i32 = ctx.step("step_b", || {
            effects.set(effects.get() + 1);
            Ok(a + 5)
        })?;
        let c: i32 = ctx.step("step_c", || {
            effects.set(effects.get() + 1);
            Ok(b * 2)
        })?;
        Ok(c)
    }

    #[test]
    fn fresh_step_runs_closure_and_records() {
        let mut ctx = fresh();
        let out: i32 = ctx.step("add", || Ok(2 + 3)).expect("step ok");
        assert_eq!(out, 5);
        assert_eq!(ctx.steps_taken(), 1);

        let entries = ctx.journal().entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].step_name(), Some("add"));
    }

    #[test]
    fn sequential_steps_increment_step_ids() {
        let mut ctx = fresh();
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        let _ = ctx.step("b", || Ok::<_, String>(2)).expect("b");
        let _ = ctx.step("c", || Ok::<_, String>(3)).expect("c");
        let entries = ctx.journal().entries();
        assert_eq!(entries[0].step_id, StepId::new(0));
        assert_eq!(entries[1].step_id, StepId::new(1));
        assert_eq!(entries[2].step_id, StepId::new(2));
    }

    #[test]
    fn step_failure_is_recorded_and_returned() {
        let mut ctx = fresh();
        let res: Result<i32> = ctx.step("boom", || Err("kaboom".to_string()));
        match res {
            Err(DurableError::StepFailed { step, message }) => {
                assert_eq!(step, "boom");
                assert_eq!(message, "kaboom");
            }
            other => panic!("expected StepFailed, got {other:?}"),
        }
        // The failure is in the log.
        let entries = ctx.journal().entries();
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].kind, EntryKind::StepFailed { .. }));
    }

    /// Core test: run a workflow halfway with side effects, "crash", build a
    /// new context from the same journal, replay → the side effects do NOT
    /// recur, the result is the same.
    #[test]
    fn replay_does_not_repeat_side_effects() {
        // Side-effect counter: each real execution of the closure increments this.
        let effects = Cell::new(0u32);

        let run_workflow = |ctx: &mut DurableContext<InMemoryJournal>| -> Result<i32> {
            let a: i32 = ctx.step("step_a", || {
                effects.set(effects.get() + 1);
                Ok(10)
            })?;
            let b: i32 = ctx.step("step_b", || {
                effects.set(effects.get() + 1);
                Ok(a + 5)
            })?;
            let c: i32 = ctx.step("step_c", || {
                effects.set(effects.get() + 1);
                Ok(b * 2)
            })?;
            Ok(c)
        };

        // --- First (full) run ---
        let journal = InMemoryJournal::new();
        let mut ctx = DurableContext::new(journal).expect("ctx 1");
        let first = run_workflow(&mut ctx).expect("first run");
        assert_eq!(first, 30);
        assert_eq!(effects.get(), 3, "three side effects in the fresh run");

        // Take the journal as if it survived a crash on disk.
        let journal = ctx.finish();
        assert_eq!(journal.len().expect("len"), 3);

        // --- Replay: new context from the same journal ---
        let mut ctx2 = DurableContext::new(journal).expect("ctx 2");
        let replayed = run_workflow(&mut ctx2).expect("replay run");

        // The result is identical AND no new side effects occurred.
        assert_eq!(replayed, first);
        assert_eq!(
            effects.get(),
            3,
            "replay must not re-run closures — no new side effects"
        );
    }

    /// Crash mid-workflow: only some steps are in the log, replay fills in the rest.
    #[test]
    fn partial_journal_resumes_from_where_it_left_off() {
        let effects = Cell::new(0u32);

        // Step 1: run only the first two steps, then "crash".
        let journal = InMemoryJournal::new();
        let mut ctx = DurableContext::new(journal).expect("ctx");
        let _ = ctx
            .step("a", || {
                effects.set(effects.get() + 1);
                Ok::<_, String>(1)
            })
            .expect("a");
        let _ = ctx
            .step("b", || {
                effects.set(effects.get() + 1);
                Ok::<_, String>(2)
            })
            .expect("b");
        let journal = ctx.finish();
        assert_eq!(effects.get(), 2);
        assert_eq!(journal.len().expect("len"), 2);

        // Step 2: continue — a and b replay from the log (no side effect),
        // c runs fresh (one new side effect).
        let mut ctx2 = DurableContext::new(journal).expect("ctx 2");
        assert!(ctx2.is_replaying());
        let a: i32 = ctx2
            .step("a", || {
                effects.set(effects.get() + 1);
                Ok(1)
            })
            .expect("a replay");
        let b: i32 = ctx2
            .step("b", || {
                effects.set(effects.get() + 1);
                Ok(2)
            })
            .expect("b replay");
        assert!(!ctx2.is_replaying(), "the cursor passed the recorded rows");
        let c: i32 = ctx2
            .step("c", || {
                effects.set(effects.get() + 1);
                Ok(a + b)
            })
            .expect("c fresh");
        assert_eq!(c, 3);
        // a+b replayed (0 new), c fresh (+1) → total 3.
        assert_eq!(effects.get(), 3);
    }

    #[test]
    fn nondeterministic_step_name_is_detected() {
        // The log has step "a"; the replaying code requests "b" at the same position.
        let mut ctx = fresh();
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        let journal = ctx.finish();

        let mut ctx2 = DurableContext::new(journal).expect("ctx2");
        let err = ctx2
            .step::<i32, _>("b", || Ok(1))
            .expect_err("name mismatch must error");
        match err {
            DurableError::NondeterministicReplay {
                index,
                expected,
                found,
            } => {
                assert_eq!(index, 0);
                assert_eq!(expected, "b");
                assert_eq!(found, "a");
            }
            other => panic!("expected NondeterministicReplay, got {other:?}"),
        }
    }

    #[test]
    fn recorded_failure_replays_as_failure_without_rerun() {
        let ran = Cell::new(false);
        // Fresh run: the step fails, is recorded as an error.
        let journal = InMemoryJournal::new();
        let mut ctx = DurableContext::new(journal).expect("ctx");
        let _ = ctx.step::<i32, _>("risky", || Err("nope".to_string()));
        let journal = ctx.finish();

        // Replay: the same step returns the same error without running the closure.
        let mut ctx2 = DurableContext::new(journal).expect("ctx2");
        let res: Result<i32> = ctx2.step("risky", || {
            ran.set(true);
            Ok(99)
        });
        assert!(!ran.get(), "a failed step must not be re-run during replay");
        match res {
            Err(DurableError::StepFailed { message, .. }) => assert_eq!(message, "nope"),
            other => panic!("expected StepFailed, got {other:?}"),
        }
    }

    #[test]
    fn finish_consumes_context_and_returns_journal() {
        // finish() transfers ownership to the journal, so the context can no
        // longer be used (a compile-time guarantee, not a runtime flag).
        let mut ctx = fresh();
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        let journal = ctx.finish();
        assert_eq!(journal.len().expect("len"), 1);
    }

    #[test]
    fn snapshot_does_not_consume_step_cursor() {
        let mut ctx = fresh();
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        ctx.snapshot(&serde_json::json!({"acc": 1}))
            .expect("snapshot");
        let _ = ctx.step("b", || Ok::<_, String>(2)).expect("b");

        // In the log: a (step0), snapshot, b (step1). Snapshot does not consume the cursor.
        let entries = ctx.journal().entries();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].step_name(), Some("a"));
        assert!(entries[1].kind.is_snapshot());
        assert_eq!(entries[2].step_name(), Some("b"));
        assert_eq!(entries[2].step_id, StepId::new(1));
        assert_eq!(ctx.steps_taken(), 2);
    }

    #[test]
    fn snapshot_is_ignored_during_replay_cursor() {
        // A snapshot between steps in the log must not break replay's name matching.
        let mut ctx = fresh();
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        ctx.snapshot(&serde_json::json!({"x": 1})).expect("snap");
        let _ = ctx.step("b", || Ok::<_, String>(2)).expect("b");
        let journal = ctx.finish();

        // Replay: a and b replay correctly even with a snapshot in between in the log.
        let mut ctx2 = DurableContext::new(journal).expect("ctx2");
        let a: i32 = ctx2.step("a", || Ok(0)).expect("a replay");
        let b: i32 = ctx2.step("b", || Ok(0)).expect("b replay");
        assert_eq!(a, 1);
        assert_eq!(b, 2);
    }

    #[test]
    fn marker_in_log_does_not_consume_step_cursor_or_break_replay() {
        // A log with step "a", a marker (non-step), step "b". The marker must
        // NOT show up in the replay cursor nor cause NondeterministicReplay.
        let mut ctx = fresh();
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        ctx.journal
            .append(JournalEntry::marker(
                StepId::new(99),
                "memory_contradicted",
                serde_json::json!({"memory": "x"}),
            ))
            .expect("append marker");
        let _ = ctx.step("b", || Ok::<_, String>(2)).expect("b");
        let journal = ctx.finish();

        // The log has three rows but only two steps.
        assert_eq!(journal.len().expect("len"), 3);

        // Replay: a and b replay correctly even with a marker in between in the log.
        let mut ctx2 = DurableContext::new(journal).expect("ctx2");
        // The replay cursor sees only two steps (the marker is filtered out).
        assert!(ctx2.is_replaying());
        let a: i32 = ctx2.step("a", || Ok(0)).expect("a replay");
        let b: i32 = ctx2.step("b", || Ok(0)).expect("b replay");
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert!(!ctx2.is_replaying());
        assert_eq!(ctx2.steps_taken(), 2);
    }

    #[test]
    fn replayed_turn_count_counts_top_level_turns_only() {
        // Simulate an agent's turn log: per turn, `turn-{n}` + `turn-{n}-think`.
        let mut ctx = fresh();
        let _ = ctx.step("turn-0", || Ok::<_, String>(0)).expect("t0");
        let _ = ctx
            .step("turn-0-think", || Ok::<_, String>("hi".to_string()))
            .expect("t0-think");
        let _ = ctx.step("turn-1", || Ok::<_, String>(1)).expect("t1");
        let _ = ctx
            .step("turn-1-think", || Ok::<_, String>(String::new()))
            .expect("t1-think");
        let journal = ctx.finish();

        // Rebuild → the replay vector has four steps, but only TWO top-level
        // turns. The next free turn slot = 2.
        let ctx2 = DurableContext::new(journal).expect("ctx2");
        assert_eq!(
            ctx2.replayed_turn_count(),
            2,
            "two top-level turns (turn-0, turn-1); -think sub-steps are not counted"
        );
    }

    #[test]
    fn fast_forward_replay_skips_to_live_without_rerunning() {
        // Run two steps, "crash", rebuild → replay state.
        let effects = Cell::new(0u32);
        let journal = {
            let mut ctx = fresh();
            let _ = ctx
                .step("turn-0", || {
                    effects.set(effects.get() + 1);
                    Ok::<_, String>(0)
                })
                .expect("t0");
            let _ = ctx
                .step("turn-0-think", || {
                    effects.set(effects.get() + 1);
                    Ok::<_, String>(String::new())
                })
                .expect("t0-think");
            ctx.finish()
        };
        assert_eq!(effects.get(), 2);

        let mut ctx2 = DurableContext::new(journal).expect("ctx2");
        assert!(ctx2.is_replaying(), "two steps in the log");

        // Live continuation: skip replay without re-running it.
        ctx2.fast_forward_replay();
        assert!(
            !ctx2.is_replaying(),
            "the cursor moved to the end of replay"
        );
        assert_eq!(ctx2.steps_taken(), 2);

        // The next step is FRESH (a new name) and does not fail with
        // NondeterministicReplay — it is recorded after the log at sequence
        // position 2.
        let fresh_effect = Cell::new(0u32);
        let out: i32 = ctx2
            .step("turn-1", || {
                fresh_effect.set(fresh_effect.get() + 1);
                Ok(42)
            })
            .expect("a new live step must not fail");
        assert_eq!(out, 42);
        assert_eq!(fresh_effect.get(), 1, "the new step ran exactly once");
        assert_eq!(ctx2.next_step_id(), StepId::new(3));
        // Replayed steps did not run again.
        assert_eq!(effects.get(), 2);
    }

    #[test]
    fn replayed_turn_count_is_zero_for_empty_journal() {
        // Fresh (non-persistent) path: replay is empty → next turn = 0.
        let ctx = fresh();
        assert_eq!(ctx.replayed_turn_count(), 0);
    }

    #[test]
    fn replayed_turn_count_ignores_non_turn_steps() {
        // A foreign step ("warmup") must not affect the turn counter.
        let mut ctx = fresh();
        let _ = ctx.step("warmup", || Ok::<_, String>(1)).expect("warmup");
        let _ = ctx.step("turn-0", || Ok::<_, String>(0)).expect("t0");
        let journal = ctx.finish();

        let ctx2 = DurableContext::new(journal).expect("ctx2");
        assert_eq!(
            ctx2.replayed_turn_count(),
            1,
            "only turn-0 is counted as a turn"
        );
    }

    #[test]
    fn next_step_id_tracks_cursor() {
        let mut ctx = fresh();
        assert_eq!(ctx.next_step_id(), StepId::new(0));
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        assert_eq!(ctx.next_step_id(), StepId::new(1));
    }

    /// Core integration test (review issue #8): end-to-end crash resistance
    /// as a **combination** of `DurableContext` + `FileJournal`, when the
    /// last row was truncated by a crash. After resuming, the preserved
    /// steps do not repeat side effects, the truncated step runs fresh
    /// exactly once, and the final result matches a crash-free run (= 30).
    #[test]
    fn file_journal_torn_last_line_resumes_on_correct_step() {
        use crate::file::FileJournal;
        use std::io::Write;

        let tmp = TempPath::new("torn");

        // --- Step 1: run all three steps into a FileJournal, then "crash". ---
        let effects = Cell::new(0u32);
        {
            let mut ctx =
                DurableContext::new(FileJournal::open(tmp.path()).expect("open 1")).expect("ctx 1");
            assert_eq!(three_step_workflow(&mut ctx, &effects).expect("first"), 30);
            assert_eq!(effects.get(), 3, "three fresh side effects");
        }

        // --- Crash: tear the last row (step_c): leave two intact rows +
        //     an incomplete (newline-less) fragment = the classic torn last line. ---
        {
            let contents = std::fs::read_to_string(tmp.path()).expect("read");
            let mut lines: Vec<&str> = contents.lines().collect();
            assert_eq!(lines.len(), 3, "three rows before tearing");
            lines.pop();
            let mut f = std::fs::File::create(tmp.path()).expect("recreate");
            for l in &lines {
                writeln!(f, "{l}").expect("write line");
            }
            write!(f, "{{\"step_id\":2,\"timestamp\":\"2026").expect("write partial");
            f.flush().expect("flush");
        }

        // --- Step 2: resume. step_a + step_b replay from the log (no new
        //     side effect), step_c runs fresh EXACTLY once. ---
        let resumed_effects = Cell::new(0u32);
        let mut ctx2 =
            DurableContext::new(FileJournal::open(tmp.path()).expect("open 2")).expect("ctx 2");
        assert!(ctx2.is_replaying(), "two intact steps in the log");
        let resumed = three_step_workflow(&mut ctx2, &resumed_effects).expect("resume");

        assert_eq!(resumed, 30, "the final result matches a crash-free run");
        assert_eq!(
            resumed_effects.get(),
            1,
            "only the truncated step_c ran again; step_a/step_b came from the log"
        );
    }

    #[test]
    fn complex_value_roundtrips_through_replay() {
        use serde::{Deserialize, Serialize};
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        struct Payload {
            id: u32,
            tags: Vec<String>,
        }

        let made = Payload {
            id: 7,
            tags: vec!["a".into(), "b".into()],
        };

        let journal = InMemoryJournal::new();
        let mut ctx = DurableContext::new(journal).expect("ctx");
        let out = ctx
            .step("build", || Ok::<_, String>(made.clone()))
            .expect("build");
        assert_eq!(out, made);
        let journal = ctx.finish();

        // Replay returns an identical structure parsed from the log.
        let mut ctx2 = DurableContext::new(journal).expect("ctx2");
        let replayed: Payload = ctx2
            .step("build", || {
                Ok(Payload {
                    id: 0,
                    tags: vec![],
                })
            })
            .expect("replay");
        assert_eq!(replayed, made);
    }
}
