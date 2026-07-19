//! Conflict-aware tagging (`SleepGate` model, arXiv 2603.14517).
//!
//! `drop_contradicted` ([`crate::contradiction`]) is **destructive**: it
//! immediately tombstones memories the durable journal has marked as
//! contradicted. `SleepGate` proposes a gentler, reversible intermediate
//! step: when two memories conflict, **don't remove either one
//! immediately** — *tag* both as `Conflicted` and let later consolidation
//! (or newer evidence) decide which one stays. This mirrors the family
//! value *"verify before disagreeing"* as a native feature: a conflict is a
//! signal to investigate, not a command to destroy.
//!
//! ## Why a tag and not a new lifecycle state
//! `Conflicted` is NOT a [`familyclaw_memory::MemoryStatus`] variant: the
//! lifecycle (`Active → Archived → Tombstoned`) lives in the
//! [`familyclaw_memory`] crate (outside this package) and describes
//! *persistence*, not *reliability*. Conflict is an orthogonal truth — a
//! memory can be `Active` and in conflict with another at the same time.
//! That's why the marking is done by adding the standard tag
//! [`CONFLICT_TAG`] to the memory's `tags` list; the memory otherwise
//! remains fully retrievable and untouched. Once the conflict resolves, the
//! tag can be removed without having had to restore the memory's status
//! from the grave.
//!
//! ## API
//! - [`ConflictTag`] — a machine-readable record of one detected conflict.
//! - [`is_conflicted`] — whether a memory is already tagged as conflicted.
//! - [`tag_conflict`] — tags both parties and returns a [`ConflictTag`].
//! - [`clear_conflict`] — removes the conflict tag from one memory (after
//!   resolution).

use crate::similarity::is_near_duplicate;
use familyclaw_core::{MessageId, Result, Timestamp};
use familyclaw_memory::{Memory, MemoryStore};
use serde::{Deserialize, Serialize};

/// Standard tag used to identify memories marked as conflicted in
/// [`Memory::tags`]. Generic (Layer A): no family/key-specific information.
pub const CONFLICT_TAG: &str = "conflicted";

/// A machine-readable record of one detected conflict between two memories.
///
/// `left` and `right` are the conflicting memories (the order is just a
/// stabilized representation — not meaningful). `detected` is the instant
/// the conflict was detected (given as a parameter, not from the system
/// clock — deterministic).
///
/// `ConflictTag` itself is a pure, serializable data record: it does NOT
/// mutate the store. The mutation (tagging) is done by the
/// [`tag_conflict`] function, which returns this record for auditing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConflictTag {
    /// The first party to the conflict (smaller id — stabilized order).
    pub left: MessageId,
    /// The second party to the conflict (larger id — stabilized order).
    pub right: MessageId,
    /// The instant the conflict was detected (UTC).
    pub detected: Timestamp,
}

impl ConflictTag {
    /// Builds a record from two memory ids and a detection instant.
    ///
    /// The parties are ordered deterministically (`left <= right`) so the
    /// same conflict always produces the same record regardless of
    /// argument order — auditing and deduplication stay reproducible.
    #[must_use]
    pub fn new(a: MessageId, b: MessageId, detected: Timestamp) -> Self {
        let (left, right) = if a <= b { (a, b) } else { (b, a) };
        Self {
            left,
            right,
            detected,
        }
    }

    /// Whether this record involves the given memory.
    #[must_use]
    pub fn involves(&self, id: MessageId) -> bool {
        self.left == id || self.right == id
    }
}

/// Whether a memory is already tagged as conflicted.
#[must_use]
pub fn is_conflicted(memory: &Memory) -> bool {
    has_conflict_tag(&memory.tags)
}

/// Internal helper: whether the tag list contains the conflict tag
/// (case-insensitive, matching how `cycle::merge_metadata_into` handles tags).
fn has_conflict_tag(tags: &[String]) -> bool {
    tags.iter().any(|t| t.eq_ignore_ascii_case(CONFLICT_TAG))
}

/// Tags **both** parties to a conflict as `Conflicted` without removing
/// either one, and returns the machine-readable [`ConflictTag`] record.
///
/// Idempotent: if a party is already tagged, it is not tagged a second time
/// (and no redundant write is performed). If either id is not found in the
/// store, that party is silently skipped (removed/unknown ⇒ not an error),
/// but the record is still returned — the observation holds true even if
/// the target has already been cleaned up.
///
/// **Does not touch lifecycle state or content** — only [`CONFLICT_TAG`] is
/// added to the `tags` list. The protected core is tagged the same as
/// anything else: a tag neither decays nor tombstones a memory, so marking
/// an identity anchor as a party to a conflict is harmless (unlike
/// tombstoning).
///
/// # Errors
/// [`familyclaw_core::FamilyClawError`] if reading from or updating the
/// store fails.
pub async fn tag_conflict<S>(
    store: &S,
    a: MessageId,
    b: MessageId,
    detected: Timestamp,
) -> Result<ConflictTag>
where
    S: MemoryStore + ?Sized,
{
    tag_one(store, a).await?;
    tag_one(store, b).await?;
    Ok(ConflictTag::new(a, b, detected))
}

/// Adds the conflict tag to one memory, if it doesn't already have one.
async fn tag_one<S>(store: &S, id: MessageId) -> Result<()>
where
    S: MemoryStore + ?Sized,
{
    let Some(mut memory) = store.get(id).await? else {
        return Ok(()); // unknown/removed party — skip silently
    };
    if has_conflict_tag(&memory.tags) {
        return Ok(()); // already tagged — idempotent, no redundant write
    }
    memory.tags.push(CONFLICT_TAG.to_string());
    store.update(memory).await
}

/// Removes the conflict tag from one memory (after the conflict resolves).
///
/// Returns `true` if the tag was present and was removed, `false` if the
/// memory wasn't found or had no tag. Removes all occurrences of the tag
/// (case-insensitive) if there are multiple.
///
/// # Errors
/// [`familyclaw_core::FamilyClawError`] if reading from or updating the
/// store fails.
pub async fn clear_conflict<S>(store: &S, id: MessageId) -> Result<bool>
where
    S: MemoryStore + ?Sized,
{
    let Some(mut memory) = store.get(id).await? else {
        return Ok(false);
    };
    let before = memory.tags.len();
    memory
        .tags
        .retain(|t| !t.eq_ignore_ascii_case(CONFLICT_TAG));
    if memory.tags.len() == before {
        return Ok(false); // no tag — no write
    }
    store.update(memory).await?;
    Ok(true)
}

/// Finds near-identical memory pairs and returns a [`ConflictTag`] for each
/// **without mutating anything** — a pure, side-effect-free detection function.
///
/// This is the read-only counterpart to [`tag_conflict`]: where
/// `tag_conflict` *writes* the tag to the store, this only *reports* which
/// pairs are worth investigating. It walks the memories with a nested
/// `i < j` loop (each pair once), skips non-retrievable memories
/// ([`Memory::is_retrievable`]), and compares the pair's `content` fields
/// using [`is_near_duplicate`] at the given threshold. A match produces a
/// [`ConflictTag::new`] from the two ids and the `detected` instant.
///
/// **The result is a CANDIDATE list of near-duplicate pairs to investigate,
/// NOT proven conflicts.** Similarity is lexical Jaccard (word-set overlap,
/// [`similarity`](crate::similarity)) — two memories with almost the same
/// words can still assert different things, and two memories written in
/// different words can assert the same thing. Use the result as *input* to
/// consolidation or a later evidence-based resolution, not as a command to
/// tombstone.
///
/// Ordering is deterministic: pairs are produced in input order (`i`
/// increasing on the outside, `j` on the inside), so the same input always
/// produces the same list in the same order. `detected` is given as a
/// parameter (not from the system clock), as elsewhere in this module.
#[must_use]
pub fn detect_conflicts(
    memories: &[Memory],
    threshold: f32,
    detected: Timestamp,
) -> Vec<ConflictTag> {
    let mut tags = Vec::new();
    for i in 0..memories.len() {
        if !memories[i].is_retrievable() {
            continue;
        }
        for other in memories.iter().skip(i + 1) {
            if !other.is_retrievable() {
                continue;
            }
            if is_near_duplicate(&memories[i].content, &other.content, threshold) {
                tags.push(ConflictTag::new(memories[i].id, other.id, detected));
            }
        }
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use familyclaw_memory::{ImportanceFactors, LocalJsonStore, MemoryStatus};

    /// Fixed reference instant: 2026-06-04 12:00 UTC (deterministic).
    fn at() -> Timestamp {
        Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0)
            .single()
            .expect("valid instant")
    }

    fn mem(content: &str) -> Memory {
        Memory::builder(content)
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .build()
    }

    #[test]
    fn conflict_tag_orders_parties_deterministically() {
        let a = MessageId::new();
        let b = MessageId::new();
        let t1 = ConflictTag::new(a, b, at());
        let t2 = ConflictTag::new(b, a, at());
        // Same conflict either way round → identical record.
        assert_eq!(t1, t2);
        assert!(t1.left <= t1.right);
        assert!(t1.involves(a));
        assert!(t1.involves(b));
        assert!(!t1.involves(MessageId::new()));
    }

    #[tokio::test]
    async fn tag_conflict_keeps_both_memories() {
        let store = LocalJsonStore::in_memory();
        let id_a = store.add(mem("agent_a is in city a")).await.expect("a");
        let id_b = store.add(mem("agent_a is in city b")).await.expect("b");

        let tag = tag_conflict(&store, id_a, id_b, at()).await.expect("tag");

        // CRITICAL: neither was removed, both are still active.
        let a = store.get(id_a).await.expect("g").expect("p");
        let b = store.get(id_b).await.expect("g").expect("p");
        assert_eq!(a.status, MemoryStatus::Active);
        assert_eq!(b.status, MemoryStatus::Active);
        // Both tagged as conflicted.
        assert!(is_conflicted(&a));
        assert!(is_conflicted(&b));
        // The record involves both.
        assert!(tag.involves(id_a));
        assert!(tag.involves(id_b));
        assert_eq!(tag.detected, at());
    }

    #[tokio::test]
    async fn tag_conflict_is_idempotent() {
        let store = LocalJsonStore::in_memory();
        let id_a = store.add(mem("claim x")).await.expect("a");
        let id_b = store.add(mem("claim not-x")).await.expect("b");

        tag_conflict(&store, id_a, id_b, at()).await.expect("1");
        tag_conflict(&store, id_a, id_b, at()).await.expect("2");

        // The tag appears exactly once on each (not twice).
        let a = store.get(id_a).await.expect("g").expect("p");
        let count = a
            .tags
            .iter()
            .filter(|t| t.eq_ignore_ascii_case(CONFLICT_TAG))
            .count();
        assert_eq!(count, 1, "the tag must not be added twice");
    }

    #[tokio::test]
    async fn tag_conflict_preserves_existing_tags() {
        let store = LocalJsonStore::in_memory();
        let m = Memory::builder("tagged memory")
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .tags(["work".to_string(), "milestone".to_string()])
            .build();
        let id_a = store.add(m).await.expect("a");
        let id_b = store.add(mem("other side")).await.expect("b");

        tag_conflict(&store, id_a, id_b, at()).await.expect("tag");

        let a = store.get(id_a).await.expect("g").expect("p");
        assert!(a.tags.iter().any(|t| t == "work"));
        assert!(a.tags.iter().any(|t| t == "milestone"));
        assert!(is_conflicted(&a));
    }

    #[tokio::test]
    async fn tag_conflict_ignores_unknown_ids() {
        let store = LocalJsonStore::in_memory();
        let real = store.add(mem("real one")).await.expect("real");
        let ghost = MessageId::new();

        // The other party isn't in the store → skipped silently, but the
        // record is still returned and the existing party gets tagged.
        let tag = tag_conflict(&store, real, ghost, at()).await.expect("tag");
        assert!(tag.involves(real));
        assert!(tag.involves(ghost));
        assert!(is_conflicted(
            &store.get(real).await.expect("g").expect("p")
        ));
        assert!(store.get(ghost).await.expect("g").is_none());
    }

    #[tokio::test]
    async fn clear_conflict_removes_tag() {
        let store = LocalJsonStore::in_memory();
        let id_a = store.add(mem("x")).await.expect("a");
        let id_b = store.add(mem("y")).await.expect("b");
        tag_conflict(&store, id_a, id_b, at()).await.expect("tag");
        assert!(is_conflicted(
            &store.get(id_a).await.expect("g").expect("p")
        ));

        let cleared = clear_conflict(&store, id_a).await.expect("clear");
        assert!(cleared);
        assert!(!is_conflicted(
            &store.get(id_a).await.expect("g").expect("p")
        ));
        // The other party is still tagged (clear affects one memory at a time).
        assert!(is_conflicted(
            &store.get(id_b).await.expect("g").expect("p")
        ));
    }

    #[tokio::test]
    async fn clear_conflict_on_untagged_is_noop() {
        let store = LocalJsonStore::in_memory();
        let id = store.add(mem("untagged")).await.expect("a");
        let cleared = clear_conflict(&store, id).await.expect("clear");
        assert!(!cleared, "no tag → false, no write");
        assert!(!clear_conflict(&store, MessageId::new())
            .await
            .expect("clear ghost"));
    }

    // ── detect_conflicts (pure, non-mutating detection function) ───────────

    #[test]
    fn detect_conflicts_empty_input_is_empty() {
        assert!(detect_conflicts(&[], 0.5, at()).is_empty());
    }

    #[test]
    fn detect_conflicts_no_near_dups_is_empty() {
        // Completely disjoint word sets → no pairs.
        let mems = [mem("alpha beta gamma"), mem("delta epsilon zeta")];
        assert!(detect_conflicts(&mems, 0.5, at()).is_empty());
    }

    #[test]
    fn detect_conflicts_two_near_dups_gives_one_pair() {
        let m1 = mem("agent_a shipped the release today");
        let m2 = mem("agent_a shipped the release today");
        let (id1, id2) = (m1.id, m2.id);
        let mems = [m1, m2];

        let tags = detect_conflicts(&mems, 0.8, at());
        assert_eq!(tags.len(), 1);
        // The record involves both, in stabilized order, with the correct instant.
        assert!(tags[0].involves(id1));
        assert!(tags[0].involves(id2));
        assert_eq!(tags[0], ConflictTag::new(id1, id2, at()));
        assert_eq!(tags[0].detected, at());
    }

    #[test]
    fn detect_conflicts_skips_non_retrievable() {
        // Two identical memories, but one is tombstoned → not retrievable,
        // so no pair is produced.
        let m1 = mem("agent_a is in city a");
        let mut m2 = mem("agent_a is in city a");
        assert!(m2.tombstone(), "an unprotected memory must be tombstonable");
        assert!(!m2.is_retrievable());
        let mems = [m1, m2];

        assert!(
            detect_conflicts(&mems, 0.8, at()).is_empty(),
            "the tombstoned party is skipped"
        );
    }

    #[test]
    fn detect_conflicts_threshold_gates_pairs() {
        // Jaccard("the cat sat", "the cat ran") = 0.5.
        let m1 = mem("the cat sat");
        let m2 = mem("the cat ran");
        let mems = [m1, m2];

        // At the 0.5 threshold the pair qualifies, at 0.6 it does not.
        assert_eq!(detect_conflicts(&mems, 0.5, at()).len(), 1);
        assert!(detect_conflicts(&mems, 0.6, at()).is_empty());
    }

    #[test]
    fn detect_conflicts_is_deterministic_and_ordered() {
        // Three near-identical memories → three pairs (0,1), (0,2), (1,2)
        // in input order; the same input → the same list every run.
        let m0 = mem("agent_a finished the migration");
        let m1 = mem("agent_a finished the migration");
        let m2 = mem("agent_a finished the migration");
        let (id0, id1, id2) = (m0.id, m1.id, m2.id);
        let mems = [m0, m1, m2];

        let first = detect_conflicts(&mems, 0.9, at());
        let second = detect_conflicts(&mems, 0.9, at());
        assert_eq!(first, second, "deterministic: same input → same list");
        assert_eq!(first.len(), 3);
        assert_eq!(first[0], ConflictTag::new(id0, id1, at()));
        assert_eq!(first[1], ConflictTag::new(id0, id2, at()));
        assert_eq!(first[2], ConflictTag::new(id1, id2, at()));
    }
}
