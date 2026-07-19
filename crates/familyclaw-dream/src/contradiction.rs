//! Conflict/obsolescence records over the durable journal.
//!
//! The `drop_contradicted` phase (design §2.3) removes memories that newer
//! information has invalidated (e.g. "`agent_a` is in city A" when later
//! information says "`agent_a` is in city B"). In the `FamilyClaw`
//! architecture, **the durable journal is the source of truth** (design
//! §1: *"durable carries everything; dreaming eats the durable log"*), so
//! the dream cycle doesn't guess at conflicts — it reads them from the
//! journal.
//!
//! This module provides:
//! - a standard way to **write** a conflict record to the journal
//!   ([`mark_contradicted`]),
//! - a way to **read** all marked memory ids back from the journal
//!   ([`contradicted_ids`]).
//!
//! Convention: the record is a [`familyclaw_durable::JournalEntry`] whose
//! kind is [`EntryKind::Marker`] named [`CONTRADICT_STEP`] with a JSON
//! object payload `{ "memory": "<uuid>" }`. **A marker is not a workflow
//! step**: [`DurableContext`](familyclaw_durable::DurableContext) filters
//! markers out of its replay cursor (like snapshots), so the same
//! append-only log can safely carry both durable workflows and the dream
//! cycle's conflict data — without a separate side channel and without the
//! risk of a record being interpreted as a step and triggering
//! `NondeterministicReplay`.

use std::collections::BTreeSet;

use familyclaw_core::MessageId;
use familyclaw_durable::{EntryKind, Journal, JournalEntry, StepId};

/// The marker name used to identify conflict records in the journal.
pub const CONTRADICT_STEP: &str = "memory_contradicted";

/// The JSON key carrying the contradicted memory's identifier.
const MEMORY_KEY: &str = "memory";

/// Writes a **marker record** to the journal stating that `memory` is
/// outdated/contradicted.
///
/// The record is an append-only [`EntryKind::Marker`] row: it lives in the
/// same log as durable workflows, but **does not consume the replay step
/// cursor**, so it cannot be confused with a workflow step. The row's
/// sequence position is derived from the journal's current length.
///
/// # Errors
/// Returns a [`familyclaw_durable::DurableError`] if writing to the
/// journal fails.
pub fn mark_contradicted<J: Journal>(
    journal: &mut J,
    memory: MessageId,
) -> familyclaw_durable::Result<()> {
    let step = StepId::new(journal.len()? as u64);
    let entry = JournalEntry::marker(
        step,
        CONTRADICT_STEP,
        serde_json::json!({ MEMORY_KEY: memory.to_string() }),
    );
    journal.append(entry)
}

/// Extracts the contradicted memory's identifier from a single journal
/// row, if the row is a valid conflict record.
///
/// Returns `None` if the row is not a [`CONTRADICT_STEP`] record or its
/// payload is in a form that isn't recognized (unknown ⇒ skipped, no
/// error — old/foreign rows don't crash the dream cycle).
fn id_from_entry(entry: &JournalEntry) -> Option<MessageId> {
    let EntryKind::Marker { name, payload } = &entry.kind else {
        return None;
    };
    if name != CONTRADICT_STEP {
        return None;
    }
    let raw = payload.get(MEMORY_KEY)?.as_str()?;
    raw.parse::<MessageId>().ok()
}

/// Reads all memory ids marked as contradicted from the journal.
///
/// Unknown or malformed rows are silently skipped (CLAUDE.md: don't crash
/// on foreign data). The result is **deduplicated** and deterministically
/// ordered ([`BTreeSet`]), so the dream cycle is reproducible.
///
/// # Errors
/// Returns a [`familyclaw_durable::DurableError`] if the journal cannot be
/// read.
pub fn contradicted_ids(
    journal: &(dyn Journal + Send + Sync),
) -> familyclaw_durable::Result<BTreeSet<MessageId>> {
    let entries = journal.replay_all()?;
    Ok(entries.iter().filter_map(id_from_entry).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_durable::InMemoryJournal;
    use serde_json::json;

    #[test]
    fn mark_then_read_roundtrip() {
        let mut journal = InMemoryJournal::new();
        let a = MessageId::new();
        let b = MessageId::new();
        mark_contradicted(&mut journal, a).expect("mark a");
        mark_contradicted(&mut journal, b).expect("mark b");

        let ids = contradicted_ids(&journal).expect("read");
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a));
        assert!(ids.contains(&b));
    }

    #[test]
    fn duplicates_are_deduplicated() {
        let mut journal = InMemoryJournal::new();
        let a = MessageId::new();
        mark_contradicted(&mut journal, a).expect("mark 1");
        mark_contradicted(&mut journal, a).expect("mark 2");
        let ids = contradicted_ids(&journal).expect("read");
        assert_eq!(ids.len(), 1);
        assert!(ids.contains(&a));
    }

    #[test]
    fn unrelated_entries_are_ignored() {
        let mut journal = InMemoryJournal::new();
        // A normal workflow step, not a conflict record.
        journal
            .append(JournalEntry::completed(
                StepId::ZERO,
                "do_work",
                json!({"ok": true}),
            ))
            .expect("append work");
        // Snapshot.
        journal
            .append(JournalEntry::snapshot(StepId::new(1), json!({"state": 1})))
            .expect("append snapshot");
        // The actual record.
        let real = MessageId::new();
        mark_contradicted(&mut journal, real).expect("mark");

        let ids = contradicted_ids(&journal).expect("read");
        assert_eq!(ids.len(), 1);
        assert!(ids.contains(&real));
    }

    #[test]
    fn malformed_marker_payload_is_skipped() {
        let journal = InMemoryJournal::new();
        // Correct marker name but wrong payload (no "memory" key).
        journal
            .append(JournalEntry::marker(
                StepId::ZERO,
                CONTRADICT_STEP,
                json!({"wrong": "shape"}),
            ))
            .expect("append");
        // Correct marker name, "memory" is not a valid uuid.
        journal
            .append(JournalEntry::marker(
                StepId::new(1),
                CONTRADICT_STEP,
                json!({"memory": "not-a-uuid"}),
            ))
            .expect("append");

        let ids = contradicted_ids(&journal).expect("read");
        assert!(ids.is_empty());
    }

    #[test]
    fn step_named_like_marker_is_not_a_contradiction() {
        // A workflow step whose name happens to be CONTRADICT_STEP is NOT
        // a conflict record — only `EntryKind::Marker` counts.
        let journal = InMemoryJournal::new();
        journal
            .append(JournalEntry::completed(
                StepId::ZERO,
                CONTRADICT_STEP,
                json!({"memory": MessageId::new().to_string()}),
            ))
            .expect("append step");
        let ids = contradicted_ids(&journal).expect("read");
        assert!(ids.is_empty(), "a step must not be interpreted as a marker");
    }

    #[test]
    fn empty_journal_has_no_contradictions() {
        let journal = InMemoryJournal::new();
        let ids = contradicted_ids(&journal).expect("read");
        assert!(ids.is_empty());
    }

    /// Regression (review issue #3): when **the same shared log** carries
    /// both a durable workflow and a conflict record, building a
    /// [`DurableContext`] on top of the log and replaying the workflow must
    /// NOT interpret the marker row as a workflow step (no
    /// `NondeterministicReplay` error), and memory-recording side effects
    /// must not re-run during replay.
    #[test]
    fn durable_context_replay_ignores_contradiction_marker_in_shared_log() {
        use familyclaw_durable::DurableContext;
        use std::cell::Cell;

        let effects = Cell::new(0u32);
        let run = |ctx: &mut DurableContext<InMemoryJournal>| -> familyclaw_durable::Result<i32> {
            let a: i32 = ctx.step("step_a", || {
                effects.set(effects.get() + 1);
                Ok(1)
            })?;
            let b: i32 = ctx.step("step_b", || {
                effects.set(effects.get() + 1);
                Ok(a + 1)
            })?;
            Ok(b)
        };

        // Fresh run: two steps into the log.
        let mut ctx = DurableContext::new(InMemoryJournal::new()).expect("ctx");
        let first = run(&mut ctx).expect("first run");
        assert_eq!(first, 2);
        assert_eq!(effects.get(), 2);

        // Dreaming writes a conflict record to the SAME log, between the steps.
        let mut journal = ctx.finish();
        mark_contradicted(&mut journal, MessageId::new()).expect("mark");

        // Rebuild the context on top of the shared log and re-run the
        // workflow: the marker must NOT break the replay cursor.
        let mut resumed = DurableContext::new(journal).expect("resume ctx");
        let replayed = run(&mut resumed).expect("replay must not mis-step on marker");
        assert_eq!(replayed, first, "replay result identical");
        assert_eq!(
            effects.get(),
            2,
            "replay ei saa ajaa askelten sulkimia uudelleen (marker ohitetaan)"
        );

        // And the record is still readable as a conflict.
        let ids = contradicted_ids(resumed.journal()).expect("ids");
        assert_eq!(ids.len(), 1);
    }
}
